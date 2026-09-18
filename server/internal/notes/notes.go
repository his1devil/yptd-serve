// Package notes is the bug-and-request log behind the Dummy agent.
//
// 每条记录是一个 Markdown 文件，目录就是数据库。选文件而不是 Mongo，是因为读它
// 的是 opencode——一个操作文件的 agent，read/grep 是它的母语；放库里反而要给它
// 做工具。文件进 git，历史、diff 白送。
//
// 但 agent 只能读，写必须经过这个包（由 yptd-note 命令暴露）。原因有三：编号要
// 不撞，格式要一致，附件要真的落盘。让模型自己写文件，这三件事都靠它自觉，而
// 「口头说记下了、磁盘上没东西」正是大模型最常见的假动作。
//
// 并发：两个群同时 @Dummy，bot 是并行处理的。所以永远不改共享文件——没有 index，
// 列表靠扫目录现算；编号带时间加随机尾巴，天然不撞。
package notes

import (
	"bufio"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
	"unicode"
)

// Kinds 是三种分类。拿不准就标待定——猜错一个 bug/需求比多一个待定糟糕。
var Kinds = []string{"bug", "需求", "待定"}

// Statuses 是记录的生命周期。
var Statuses = []string{"open", "done", "wontfix"}

// Attachment 是随消息附的一个文件。URL 是 OpenIM 的 /object/ 永久链接（每次
// 访问它现签一个短时效的 MinIO 链接跳过去），所以链接本身不过期；Local 是下载
// 到 assets/ 的备份，下载失败留空，记录照样成立。
type Attachment struct {
	Name  string
	URL   string
	Local string
	Bytes int64
}

// Note 是一条记录。
type Note struct {
	ID          string
	Kind        string
	Status      string
	Title       string
	From        string // "昵称 (userID)"
	Where       string // "#群名 (groupID)"，私聊留空
	Msg         string // 原消息的 clientMsgID，能跳回去
	At          time.Time
	Attachments []Attachment
	Body        string // frontmatter 之后的全部正文
	Path        string // 相对 root 的路径，读出来时填
}

// Store 是一个记录目录。
type Store struct {
	Root string
	// Now 可替换，测试用。
	Now func() time.Time
	// Fetch 下载附件；nil 用默认的 http 客户端。测试里换成假的。
	Fetch func(ctx context.Context, url string) (io.ReadCloser, int64, error)
	// MaxAttachment 是单个附件的下载上限，超过只记链接。默认 200MB：录屏能到
	// 这个量级，再大就不该塞进 bug 记录里了。
	MaxAttachment int64
	// Git 为 false 时不提交。目录不是 git 仓库时自动为 false。
	Git bool
}

// Open 打开一个目录；不存在就建。
func Open(root string) (*Store, error) {
	if root == "" {
		return nil, errors.New("notes: 目录为空")
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		return nil, fmt.Errorf("notes: %w", err)
	}
	s := &Store{Root: root, Now: time.Now, MaxAttachment: 200 << 20}
	if _, err := os.Stat(filepath.Join(root, ".git")); err == nil {
		s.Git = true
	}
	return s, nil
}

// Init 把目录变成一个能用的记录本：README、.gitignore、git init。重复调用无害。
func (s *Store) Init(ctx context.Context) error {
	if err := os.MkdirAll(filepath.Join(s.Root, "assets"), 0o755); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(s.Root, "README.md")); os.IsNotExist(err) {
		if err := os.WriteFile(filepath.Join(s.Root, "README.md"), []byte(readme), 0o644); err != nil {
			return err
		}
	}
	// 附件不进 git：截图录屏动辄几十 MB，进了仓库就再也瘦不回去。
	if _, err := os.Stat(filepath.Join(s.Root, ".gitignore")); os.IsNotExist(err) {
		if err := os.WriteFile(filepath.Join(s.Root, ".gitignore"), []byte("assets/\n"), 0o644); err != nil {
			return err
		}
	}
	if _, err := os.Stat(filepath.Join(s.Root, ".git")); os.IsNotExist(err) {
		if out, err := s.git(ctx, "init", "-q"); err != nil {
			return fmt.Errorf("git init: %v: %s", err, out)
		}
	}
	s.Git = true
	return s.commit(ctx, "notes: init", "README.md", ".gitignore")
}

// Add 写一条新记录，返回它。body 是用户的原话，逐字保存。
func (s *Store) Add(ctx context.Context, n Note, body string) (Note, error) {
	if !contains(Kinds, n.Kind) {
		n.Kind = "待定"
	}
	n.Status = "open"
	n.At = s.Now()
	n.Title = strings.TrimSpace(n.Title)
	if n.Title == "" {
		n.Title = firstLine(body)
	}
	if n.Title == "" {
		n.Title = "（无标题）"
	}
	id, err := s.newID(n.At)
	if err != nil {
		return Note{}, err
	}
	n.ID = id
	n.Path = filepath.Join(n.At.Format("2006/01"), id+"-"+slug(n.Title)+".md")

	for i := range n.Attachments {
		s.fetch(ctx, id, &n.Attachments[i])
	}

	var b strings.Builder
	b.WriteString("## 原话\n\n")
	b.WriteString(strings.TrimRight(body, "\n"))
	b.WriteString("\n")
	if len(n.Attachments) > 0 {
		b.WriteString("\n## 附件\n\n")
		for _, a := range n.Attachments {
			if a.Local != "" {
				fmt.Fprintf(&b, "- %s（%s，%s）\n", a.Local, a.Name, human(a.Bytes))
			} else {
				fmt.Fprintf(&b, "- %s（未下载，链接见上）\n", a.Name)
			}
		}
	}
	n.Body = b.String()

	if err := s.write(n); err != nil {
		return Note{}, err
	}
	if err := s.commit(ctx, fmt.Sprintf("note: %s %s", id, n.Title), n.Path); err != nil {
		return Note{}, err
	}
	return n, nil
}

// Append 往一条已有记录后面补一段。谁补的、什么时候，写在小标题里。
func (s *Store) Append(ctx context.Context, id, from, text string) (Note, error) {
	n, err := s.Get(id)
	if err != nil {
		return Note{}, err
	}
	n.Body = strings.TrimRight(n.Body, "\n") + fmt.Sprintf("\n\n## 补充 · %s · %s\n\n%s\n",
		s.Now().Format("2006-01-02 15:04"), from, strings.TrimSpace(text))
	if err := s.write(n); err != nil {
		return Note{}, err
	}
	return n, s.commit(ctx, fmt.Sprintf("note: %s 补充", id), n.Path)
}

// Set 改 status 或 kind。别的字段不给改：原话和来源是证据，不是可编辑的。
func (s *Store) Set(ctx context.Context, id, key, value string) (Note, error) {
	n, err := s.Get(id)
	if err != nil {
		return Note{}, err
	}
	switch key {
	case "status":
		if !contains(Statuses, value) {
			return Note{}, fmt.Errorf("status 只能是 %s", strings.Join(Statuses, "/"))
		}
		n.Status = value
	case "kind":
		if !contains(Kinds, value) {
			return Note{}, fmt.Errorf("kind 只能是 %s", strings.Join(Kinds, "/"))
		}
		n.Kind = value
	default:
		return Note{}, fmt.Errorf("只能改 status 和 kind，不能改 %s", key)
	}
	if err := s.write(n); err != nil {
		return Note{}, err
	}
	return n, s.commit(ctx, fmt.Sprintf("note: %s %s=%s", id, key, value), n.Path)
}

// Get 按 id 读一条。
func (s *Store) Get(id string) (Note, error) {
	if !idRe.MatchString(id) {
		// 也是安全门：id 只可能是这个形状，拼不出 ../ 之类的路径。
		return Note{}, fmt.Errorf("不是记录编号：%q", id)
	}
	matches, _ := filepath.Glob(filepath.Join(s.Root, "20*", "*", id+"-*.md"))
	if len(matches) == 0 {
		return Note{}, fmt.Errorf("没有 #%s 这条记录", id)
	}
	return s.read(matches[0])
}

// Filter 是 List 的筛选条件；零值全要。
type Filter struct {
	Status string
	Kind   string
	From   string    // 子串匹配昵称
	Since  time.Time // 零值不限
}

// List 扫目录，新的在前。
func (s *Store) List(f Filter) ([]Note, error) {
	matches, _ := filepath.Glob(filepath.Join(s.Root, "20*", "*", "*.md"))
	var out []Note
	for _, p := range matches {
		n, err := s.read(p)
		if err != nil {
			continue // 一个坏文件不该让整张列表打不开
		}
		if f.Status != "" && n.Status != f.Status {
			continue
		}
		if f.Kind != "" && n.Kind != f.Kind {
			continue
		}
		if f.From != "" && !strings.Contains(n.From, f.From) {
			continue
		}
		if !f.Since.IsZero() && n.At.Before(f.Since) {
			continue
		}
		out = append(out, n)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].At.After(out[j].At) })
	return out, nil
}

// --- 编号 ---

// 编号是 MMDD-HHMM-xx：读得出是哪天记的，两位随机尾巴防同一分钟撞车。年份在目录
// 里。不用自增，是因为两个进程同时数文件必然撞号，而一把锁又得有人负责释放。
var idRe = regexp.MustCompile(`^\d{4}-\d{4}-[a-z0-9]{2}$`)

func (s *Store) newID(at time.Time) (string, error) {
	const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789"
	for try := 0; try < 20; try++ {
		var r [2]byte
		if _, err := rand.Read(r[:]); err != nil {
			return "", err
		}
		id := fmt.Sprintf("%s-%c%c", at.Format("0102-1504"), alphabet[int(r[0])%len(alphabet)], alphabet[int(r[1])%len(alphabet)])
		if m, _ := filepath.Glob(filepath.Join(s.Root, "20*", "*", id+"-*.md")); len(m) == 0 {
			return id, nil
		}
	}
	return "", errors.New("notes: 一分钟内编号用尽（这不该发生）")
}

// slug 把标题变成文件名里能用的一段：中文留着（文件名就该看得懂），空白和符号
// 变 -，最多 40 个字符。
func slug(title string) string {
	var b strings.Builder
	dash := false
	for _, r := range title {
		switch {
		case unicode.IsLetter(r) || unicode.IsDigit(r):
			b.WriteRune(r)
			dash = false
		case !dash && b.Len() > 0:
			b.WriteByte('-')
			dash = true
		}
		if b.Len() >= 40 {
			break
		}
	}
	return strings.TrimRight(b.String(), "-")
}

// --- 附件 ---

func (s *Store) fetch(ctx context.Context, id string, a *Attachment) {
	a.Name = safeName(a.Name)
	if a.URL == "" {
		return
	}
	fetch := s.Fetch
	if fetch == nil {
		fetch = httpFetch
	}
	rc, size, err := fetch(ctx, a.URL)
	if err != nil {
		return
	}
	defer rc.Close()
	if size > s.MaxAttachment {
		return
	}
	dir := filepath.Join(s.Root, "assets", id)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return
	}
	dst := filepath.Join(dir, a.Name)
	f, err := os.Create(dst)
	if err != nil {
		return
	}
	// 上限再守一次：Content-Length 可能没给或者说谎。
	n, err := io.Copy(f, io.LimitReader(rc, s.MaxAttachment+1))
	f.Close()
	if err != nil || n > s.MaxAttachment {
		os.Remove(dst)
		return
	}
	a.Local = filepath.ToSlash(filepath.Join("assets", id, a.Name))
	a.Bytes = n
}

func httpFetch(ctx context.Context, url string) (io.ReadCloser, int64, error) {
	// 录屏几十 MB 走 2Mbps 的出口要几分钟，超时不能按网页的标准给。
	ctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		cancel()
		return nil, 0, err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		cancel()
		return nil, 0, err
	}
	if resp.StatusCode != http.StatusOK {
		resp.Body.Close()
		cancel()
		return nil, 0, fmt.Errorf("http %d", resp.StatusCode)
	}
	return struct {
		io.Reader
		io.Closer
	}{resp.Body, closerFunc(func() error { cancel(); return resp.Body.Close() })}, resp.ContentLength, nil
}

type closerFunc func() error

func (f closerFunc) Close() error { return f() }

// safeName 只留文件名本身：去掉路径、去掉 ..，空的给个占位。
func safeName(name string) string {
	name = filepath.Base(strings.ReplaceAll(name, "\\", "/"))
	name = strings.Trim(name, ". ")
	if name == "" || name == "/" {
		return "attachment"
	}
	return name
}

// --- 文件读写 ---

func (s *Store) write(n Note) error {
	full := filepath.Join(s.Root, n.Path)
	if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
		return err
	}
	var b strings.Builder
	b.WriteString("---\n")
	fmt.Fprintf(&b, "id: %s\n", n.ID)
	fmt.Fprintf(&b, "kind: %s\n", n.Kind)
	fmt.Fprintf(&b, "status: %s\n", n.Status)
	fmt.Fprintf(&b, "title: %s\n", oneLine(n.Title))
	fmt.Fprintf(&b, "from: %s\n", oneLine(n.From))
	fmt.Fprintf(&b, "where: %s\n", oneLine(n.Where))
	fmt.Fprintf(&b, "at: %s\n", n.At.Format(time.RFC3339))
	fmt.Fprintf(&b, "msg: %s\n", oneLine(n.Msg))
	if len(n.Attachments) > 0 {
		b.WriteString("attachments:\n")
		for _, a := range n.Attachments {
			fmt.Fprintf(&b, "  - name: %s\n    url: %s\n", oneLine(a.Name), oneLine(a.URL))
			if a.Local != "" {
				fmt.Fprintf(&b, "    local: %s\n    bytes: %d\n", a.Local, a.Bytes)
			}
		}
	}
	b.WriteString("---\n\n")
	b.WriteString(n.Body)
	// 先写临时文件再改名：读的人永远看不到半个文件。
	tmp := full + ".tmp"
	if err := os.WriteFile(tmp, []byte(b.String()), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, full)
}

func (s *Store) read(full string) (Note, error) {
	f, err := os.Open(full)
	if err != nil {
		return Note{}, err
	}
	defer f.Close()
	rel, _ := filepath.Rel(s.Root, full)
	n := Note{Path: filepath.ToSlash(rel)}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<24)
	if !sc.Scan() || sc.Text() != "---" {
		return Note{}, fmt.Errorf("%s: 没有 frontmatter", rel)
	}
	var cur *Attachment
	for sc.Scan() {
		line := sc.Text()
		if line == "---" {
			break
		}
		if strings.HasPrefix(line, "  - name: ") {
			n.Attachments = append(n.Attachments, Attachment{Name: strings.TrimPrefix(line, "  - name: ")})
			cur = &n.Attachments[len(n.Attachments)-1]
			continue
		}
		if cur != nil && strings.HasPrefix(line, "    ") {
			k, v, _ := strings.Cut(strings.TrimSpace(line), ": ")
			switch k {
			case "url":
				cur.URL = v
			case "local":
				cur.Local = v
			case "bytes":
				fmt.Sscan(v, &cur.Bytes)
			}
			continue
		}
		cur = nil
		k, v, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		v = strings.TrimSpace(v)
		switch k {
		case "id":
			n.ID = v
		case "kind":
			n.Kind = v
		case "status":
			n.Status = v
		case "title":
			n.Title = v
		case "from":
			n.From = v
		case "where":
			n.Where = v
		case "msg":
			n.Msg = v
		case "at":
			n.At, _ = time.Parse(time.RFC3339, v)
		}
	}
	var body strings.Builder
	first := true
	for sc.Scan() {
		if first && sc.Text() == "" {
			first = false
			continue
		}
		first = false
		body.WriteString(sc.Text())
		body.WriteByte('\n')
	}
	n.Body = body.String()
	return n, sc.Err()
}

// --- git ---

func (s *Store) git(ctx context.Context, args ...string) (string, error) {
	// 身份写死在命令里：跑这个的是 yptdbot 这种服务账号，没有也不该有全局 git 配置。
	full := append([]string{"-c", "user.name=yptd-note", "-c", "user.email=yptd-note@localhost"}, args...)
	cmd := exec.CommandContext(ctx, "git", full...)
	cmd.Dir = s.Root
	out, err := cmd.CombinedOutput()
	return strings.TrimSpace(string(out)), err
}

func (s *Store) commit(ctx context.Context, msg string, paths ...string) error {
	if !s.Git {
		return nil
	}
	// 两条记录同时提交会抢 index.lock。等一等再试，而不是让第二条失败——文件
	// 已经落盘了，提交只是留历史。
	var lastErr error
	for try := 0; try < 5; try++ {
		if out, err := s.git(ctx, append([]string{"add", "--"}, paths...)...); err != nil {
			lastErr = fmt.Errorf("git add: %v: %s", err, out)
		} else if out, err := s.git(ctx, "commit", "-q", "--allow-empty", "-m", msg); err != nil {
			lastErr = fmt.Errorf("git commit: %v: %s", err, out)
		} else {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(time.Duration(200*(try+1)) * time.Millisecond):
		}
	}
	return lastErr
}

// --- 小工具 ---

func contains(list []string, v string) bool {
	for _, x := range list {
		if x == v {
			return true
		}
	}
	return false
}

func firstLine(s string) string {
	for _, line := range strings.Split(s, "\n") {
		if t := strings.TrimSpace(line); t != "" {
			if len([]rune(t)) > 40 {
				return string([]rune(t)[:40])
			}
			return t
		}
	}
	return ""
}

func oneLine(s string) string {
	return strings.Join(strings.Fields(s), " ")
}

func human(n int64) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1fMB", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%dKB", n>>10)
	}
	return fmt.Sprintf("%dB", n)
}

const readme = `# yptd 记录本

bug 和需求，一条一个文件，按 年/月 归档。文件名 = 编号-标题。

读：直接 grep / cat，或 ` + "`yptd-note list` / `yptd-note show <id>`" + `。
写：**只用 ` + "`yptd-note`" + `**。它负责编号、格式、附件下载和 git 提交。手改文件会撞号、丢附件。

编号 ` + "`MMDD-HHMM-xx`" + `，年份看目录。

kind：bug（现在的行为和预期不符）/ 需求（现在没有、希望有）/ 待定（说不清）
status：open / done / wontfix

附件在 assets/<id>/，不进 git。
`
