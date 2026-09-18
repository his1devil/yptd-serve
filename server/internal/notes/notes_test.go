package notes

import (
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func testStore(t *testing.T) *Store {
	t.Helper()
	s, err := Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	at := time.Date(2026, 9, 17, 20, 31, 0, 0, time.FixedZone("CST", 8*3600))
	s.Now = func() time.Time { return at }
	return s
}

func TestAddWritesOneFilePerNoteAndReadsItBack(t *testing.T) {
	s := testStore(t)
	n, err := s.Add(context.Background(), Note{
		Kind: "bug", From: "阿森 (asen)", Where: "#Skywalker (4134911482)", Msg: "cmid-1",
	}, "登录后头像不刷新\n要重启才行")
	if err != nil {
		t.Fatal(err)
	}
	if !idRe.MatchString(n.ID) || !strings.HasPrefix(n.ID, "0917-2031-") {
		t.Fatalf("id 该是 MMDD-HHMM-xx，得到 %q", n.ID)
	}
	if !strings.HasPrefix(n.Path, "2026/09/"+n.ID+"-登录后头像不刷新") {
		t.Fatalf("文件该按年月归档、名字带标题，得到 %q", n.Path)
	}
	got, err := s.Get(n.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Kind != "bug" || got.Status != "open" || got.From != "阿森 (asen)" || got.Msg != "cmid-1" {
		t.Fatalf("frontmatter 没读回来：%+v", got)
	}
	if !got.At.Equal(s.Now()) {
		t.Fatalf("时间没保住：%v", got.At)
	}
	if !strings.Contains(got.Body, "## 原话\n\n登录后头像不刷新\n要重启才行") {
		t.Fatalf("原话该逐字保存：%q", got.Body)
	}
}

func TestUnknownKindBecomesPending(t *testing.T) {
	// 模型分类分错比多一个待定糟糕，所以不认识的 kind 不报错，落成待定。
	s := testStore(t)
	n, err := s.Add(context.Background(), Note{Kind: "feature"}, "希望有暗色主题")
	if err != nil {
		t.Fatal(err)
	}
	if n.Kind != "待定" {
		t.Fatalf("kind = %q", n.Kind)
	}
	if n.Title != "希望有暗色主题" {
		t.Fatalf("没给标题该拿正文第一行，得到 %q", n.Title)
	}
}

func TestListFiltersAndOrdersNewestFirst(t *testing.T) {
	s := testStore(t)
	ctx := context.Background()
	base := s.Now()
	s.Now = func() time.Time { return base }
	a, _ := s.Add(ctx, Note{Kind: "bug", From: "阿森 (asen)"}, "第一条")
	s.Now = func() time.Time { return base.Add(time.Minute) }
	b, _ := s.Add(ctx, Note{Kind: "需求", From: "Luke (luke)"}, "第二条")
	if _, err := s.Set(ctx, a.ID, "status", "done"); err != nil {
		t.Fatal(err)
	}

	all, _ := s.List(Filter{})
	if len(all) != 2 || all[0].ID != b.ID {
		t.Fatalf("该两条、新的在前：%+v", ids(all))
	}
	open, _ := s.List(Filter{Status: "open"})
	if len(open) != 1 || open[0].ID != b.ID {
		t.Fatalf("status 筛选错了：%v", ids(open))
	}
	bugs, _ := s.List(Filter{Kind: "bug"})
	if len(bugs) != 1 || bugs[0].ID != a.ID {
		t.Fatalf("kind 筛选错了：%v", ids(bugs))
	}
	byFrom, _ := s.List(Filter{From: "Luke"})
	if len(byFrom) != 1 || byFrom[0].ID != b.ID {
		t.Fatalf("from 筛选错了：%v", ids(byFrom))
	}
	since, _ := s.List(Filter{Since: base.Add(30 * time.Second)})
	if len(since) != 1 || since[0].ID != b.ID {
		t.Fatalf("since 筛选错了：%v", ids(since))
	}
}

func TestSetOnlyTouchesStatusAndKind(t *testing.T) {
	s := testStore(t)
	ctx := context.Background()
	n, _ := s.Add(ctx, Note{Kind: "待定"}, "不知道算啥")
	if _, err := s.Set(ctx, n.ID, "status", "closed"); err == nil {
		t.Fatal("closed 不是合法状态，该拒绝")
	}
	if _, err := s.Set(ctx, n.ID, "title", "改标题"); err == nil {
		t.Fatal("标题是证据，不该给改")
	}
	got, err := s.Set(ctx, n.ID, "kind", "bug")
	if err != nil || got.Kind != "bug" {
		t.Fatalf("kind 该能改：%v %+v", err, got)
	}
	again, _ := s.Get(n.ID)
	if again.Kind != "bug" || !strings.Contains(again.Body, "不知道算啥") {
		t.Fatalf("改完 frontmatter 正文该原样：%+v", again)
	}
}

func TestAppendKeepsOriginalAndDatesTheAddition(t *testing.T) {
	s := testStore(t)
	ctx := context.Background()
	n, _ := s.Add(ctx, Note{Kind: "bug"}, "第一次说的")
	got, err := s.Append(ctx, n.ID, "Luke (luke)", "我也遇到了，iOS 上一样")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(got.Body, "第一次说的") || !strings.Contains(got.Body, "## 补充 · 2026-09-17 20:31 · Luke (luke)\n\n我也遇到了") {
		t.Fatalf("补充该带时间和人、原话不动：%q", got.Body)
	}
}

func TestGetRejectsAnythingThatIsNotAnID(t *testing.T) {
	// id 是唯一从模型那里进来会拼进路径的东西，形状必须卡死。
	s := testStore(t)
	for _, bad := range []string{"../README", "0917-2031-a3/../../x", "README.md", "", "0917-2031-A3"} {
		if _, err := s.Get(bad); err == nil || !strings.Contains(err.Error(), "不是记录编号") {
			t.Fatalf("%q 该被拒：%v", bad, err)
		}
	}
}

func TestAttachmentsDownloadToAssetsAndFailureStillRecords(t *testing.T) {
	s := testStore(t)
	s.Fetch = func(_ context.Context, url string) (io.ReadCloser, int64, error) {
		switch url {
		case "https://im/object/shot.png":
			return io.NopCloser(strings.NewReader("PNGDATA")), 7, nil
		case "https://im/object/huge.mov":
			return io.NopCloser(strings.NewReader("x")), 300 << 20, nil
		}
		return nil, 0, io.ErrUnexpectedEOF
	}
	n, err := s.Add(context.Background(), Note{Kind: "bug", Attachments: []Attachment{
		{Name: "../../etc/shot.png", URL: "https://im/object/shot.png"},
		{Name: "huge.mov", URL: "https://im/object/huge.mov"},
		{Name: "gone.png", URL: "https://im/object/gone.png"},
	}}, "看截图")
	if err != nil {
		t.Fatal(err)
	}
	a := n.Attachments
	if a[0].Local != "assets/"+n.ID+"/shot.png" || a[0].Bytes != 7 {
		t.Fatalf("该下到 assets/<id>/ 且文件名去掉路径：%+v", a[0])
	}
	if data, _ := os.ReadFile(filepath.Join(s.Root, a[0].Local)); string(data) != "PNGDATA" {
		t.Fatalf("文件内容不对：%q", data)
	}
	if a[1].Local != "" {
		t.Fatal("超过上限的只记链接，不下载")
	}
	if a[2].Local != "" {
		t.Fatal("下载失败的只记链接")
	}
	got, _ := s.Get(n.ID)
	if len(got.Attachments) != 3 || got.Attachments[1].URL != "https://im/object/huge.mov" || got.Attachments[0].Local != a[0].Local {
		t.Fatalf("附件列表没读回来：%+v", got.Attachments)
	}
	if !strings.Contains(got.Body, "## 附件") || !strings.Contains(got.Body, "未下载") {
		t.Fatalf("正文该有附件段：%q", got.Body)
	}
}

func TestInitMakesAGitRepoAndAddCommits(t *testing.T) {
	s := testStore(t)
	ctx := context.Background()
	if err := s.Init(ctx); err != nil {
		t.Fatal(err)
	}
	if err := s.Init(ctx); err != nil {
		t.Fatalf("重复 init 该无害：%v", err)
	}
	n, err := s.Add(ctx, Note{Kind: "bug"}, "进 git 的一条")
	if err != nil {
		t.Fatal(err)
	}
	out, err := s.git(ctx, "log", "--oneline")
	if err != nil {
		t.Fatal(err, out)
	}
	if !strings.Contains(out, "note: "+n.ID) || !strings.Contains(out, "notes: init") {
		t.Fatalf("git log 里该有 init 和这条：\n%s", out)
	}
	ignore, _ := os.ReadFile(filepath.Join(s.Root, ".gitignore"))
	if !strings.Contains(string(ignore), "assets/") {
		t.Fatal("附件不该进 git")
	}
}

func TestSlugKeepsCJKAndCaps(t *testing.T) {
	cases := map[string]string{
		"登录后头像不刷新":               "登录后头像不刷新",
		"Sidebar: agent 名字被截断!!": "Sidebar-agent-名字被截断",
		"   ":                    "",
	}
	for in, want := range cases {
		if got := slug(in); got != want {
			t.Errorf("slug(%q) = %q, want %q", in, got, want)
		}
	}
	if got := slug(strings.Repeat("长", 60)); len([]rune(got)) > 14 { // 40 字节 ≈ 13 个汉字
		t.Errorf("slug 该截断：%d 个字", len([]rune(got)))
	}
}

func ids(ns []Note) []string {
	out := make([]string, len(ns))
	for i, n := range ns {
		out[i] = n.ID
	}
	return out
}
