// yptd-note 是 Dummy（和别的 agent）记 bug、记需求的那支笔。
//
// 记录本身是 notes 目录下的 Markdown 文件，agent 可以随便 read/grep；但写只能走
// 这条命令——它保证编号不撞、格式一致、附件真的落盘、每次改动进 git。
//
// 目录取 $YPTD_NOTES_DIR，没有就 ~/notes。opencode 以 yptdbot 跑，HOME 是
// /opt/yptd-bot，所以线上就是 /opt/yptd-bot/notes。
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/his1devil/yptd/server/internal/notes"
)

const usage = `用法：
  yptd-note init
  yptd-note add --kind bug|需求|待定 --title "一句话" [--from "昵称 (id)"] [--where "#群 (id)"]
                [--msg clientMsgID] [--attach URL] [--attach-name 文件名]... < 原话
  yptd-note append <id> [--from "昵称 (id)"] < 补充
  yptd-note list [--status open|done|wontfix] [--kind bug|需求|待定] [--from 昵称] [--since 7d]
  yptd-note show <id>
  yptd-note set <id> status=done | kind=bug

目录：$YPTD_NOTES_DIR，默认 ~/notes
`

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "yptd-note:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		fmt.Fprint(os.Stderr, usage)
		return fmt.Errorf("缺子命令")
	}
	root := os.Getenv("YPTD_NOTES_DIR")
	if root == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return err
		}
		root = filepath.Join(home, "notes")
	}
	s, err := notes.Open(root)
	if err != nil {
		return err
	}
	ctx := context.Background()

	switch args[0] {
	case "init":
		if err := s.Init(ctx); err != nil {
			return err
		}
		fmt.Println("记录本就位：", root)
		return nil

	case "add":
		fs := flag.NewFlagSet("add", flag.ContinueOnError)
		var n notes.Note
		var urls, names multi
		fs.StringVar(&n.Kind, "kind", "待定", "")
		fs.StringVar(&n.Title, "title", "", "")
		fs.StringVar(&n.From, "from", "", "")
		fs.StringVar(&n.Where, "where", "", "")
		fs.StringVar(&n.Msg, "msg", "", "")
		fs.Var(&urls, "attach", "")
		fs.Var(&names, "attach-name", "")
		if err := fs.Parse(args[1:]); err != nil {
			return err
		}
		for i, u := range urls {
			a := notes.Attachment{URL: u, Name: filepath.Base(strings.SplitN(u, "?", 2)[0])}
			if i < len(names) {
				a.Name = names[i]
			}
			n.Attachments = append(n.Attachments, a)
		}
		body, err := io.ReadAll(os.Stdin)
		if err != nil {
			return err
		}
		if strings.TrimSpace(string(body)) == "" && len(n.Attachments) == 0 {
			return fmt.Errorf("原话为空（从标准输入读），也没有附件——没什么可记的")
		}
		got, err := s.Add(ctx, n, string(body))
		if err != nil {
			return err
		}
		// 只输出 id 一行：模型回复里的编号必须来自这里，别的输出只会让它有东西可编。
		fmt.Println(got.ID)
		return nil

	case "append":
		if len(args) < 2 {
			return fmt.Errorf("append 要 id")
		}
		fs := flag.NewFlagSet("append", flag.ContinueOnError)
		from := fs.String("from", "", "")
		if err := fs.Parse(args[2:]); err != nil {
			return err
		}
		body, err := io.ReadAll(os.Stdin)
		if err != nil {
			return err
		}
		if strings.TrimSpace(string(body)) == "" {
			return fmt.Errorf("补充为空（从标准输入读）")
		}
		got, err := s.Append(ctx, args[1], *from, string(body))
		if err != nil {
			return err
		}
		fmt.Println(got.ID)
		return nil

	case "list":
		fs := flag.NewFlagSet("list", flag.ContinueOnError)
		var f notes.Filter
		since := fs.String("since", "", "")
		fs.StringVar(&f.Status, "status", "", "")
		fs.StringVar(&f.Kind, "kind", "", "")
		fs.StringVar(&f.From, "from", "", "")
		if err := fs.Parse(args[1:]); err != nil {
			return err
		}
		if *since != "" {
			d, err := parseSince(*since)
			if err != nil {
				return err
			}
			f.Since = time.Now().Add(-d)
		}
		list, err := s.List(f)
		if err != nil {
			return err
		}
		if len(list) == 0 {
			fmt.Println("（没有符合的记录）")
			return nil
		}
		for _, n := range list {
			att := ""
			if len(n.Attachments) > 0 {
				att = fmt.Sprintf("  📎%d", len(n.Attachments))
			}
			// kind 列有中文，%-4s 按字节对齐会歪，自己补空格
			fmt.Printf("%s  %s %-7s %s  %s  %s%s\n", n.ID, pad(n.Kind, 4), n.Status, n.At.Format("01-02"), n.From, n.Title, att)
		}
		return nil

	case "show":
		if len(args) < 2 {
			return fmt.Errorf("show 要 id")
		}
		n, err := s.Get(args[1])
		if err != nil {
			return err
		}
		data, err := os.ReadFile(filepath.Join(root, filepath.FromSlash(n.Path)))
		if err != nil {
			return err
		}
		os.Stdout.Write(data)
		return nil

	case "set":
		if len(args) < 3 {
			return fmt.Errorf("set 要 id 和 key=value")
		}
		k, v, ok := strings.Cut(args[2], "=")
		if !ok {
			return fmt.Errorf("要 key=value 的形式，比如 status=done")
		}
		got, err := s.Set(ctx, args[1], k, v)
		if err != nil {
			return err
		}
		fmt.Printf("%s %s=%s\n", got.ID, k, v)
		return nil

	case "-h", "--help", "help":
		fmt.Print(usage)
		return nil
	}
	fmt.Fprint(os.Stderr, usage)
	return fmt.Errorf("不认识的子命令 %q", args[0])
}

// parseSince 认 7d / 24h / 30m。
func parseSince(s string) (time.Duration, error) {
	if strings.HasSuffix(s, "d") {
		days, err := strconv.Atoi(strings.TrimSuffix(s, "d"))
		if err != nil {
			return 0, fmt.Errorf("--since 要像 7d 或 24h")
		}
		return time.Duration(days) * 24 * time.Hour, nil
	}
	d, err := time.ParseDuration(s)
	if err != nil {
		return 0, fmt.Errorf("--since 要像 7d 或 24h")
	}
	return d, nil
}

// pad 按显示宽度补空格：一个汉字算两格。
func pad(s string, width int) string {
	w := 0
	for _, r := range s {
		if r > 0x2e80 {
			w += 2
		} else {
			w++
		}
	}
	if w >= width {
		return s
	}
	return s + strings.Repeat(" ", width-w)
}

type multi []string

func (m *multi) String() string     { return strings.Join(*m, ",") }
func (m *multi) Set(v string) error { *m = append(*m, v); return nil }
