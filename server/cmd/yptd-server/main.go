// Command yptd-server is both the HTTP service and its admin CLI.
//
// One binary on purpose: the CLI reaches the same store through the same code
// the service uses, so an admin command can never drift from what the running
// server believes. Administration goes over SSH rather than a web console --
// for a friend-sized deployment, the SSH key is already the access control.
package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"text/tabwriter"
	"time"

	"github.com/his1devil/yptd/server/internal/api"
	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/config"
	"github.com/his1devil/yptd/server/internal/invite"
	"github.com/his1devil/yptd/server/internal/openim"
	"github.com/his1devil/yptd/server/internal/store"
)

const usage = `yptd-server — yptd 服务端与管理工具

  yptd-server serve                        启动 HTTP 服务
  yptd-server invite new [备注] [-n 数量]   生成邀请码
  yptd-server invite list [-a]             列出邀请码（-a 含已用）
  yptd-server invite rm <码>                删除邀请码
  yptd-server user list                    列出用户
  yptd-server user disable <userID>        停用账号
  yptd-server user enable  <userID>        恢复账号
  yptd-server user revoke  <userID>        吊销该用户全部设备凭据
  yptd-server user rm      <userID>        删除账号：吊销凭据并从花名册移除
  yptd-server check                        自检：Mongo 与 OpenIM 连通性

  yptd-server bot setup                    创建 bot 账号（不占用邀请码）
  yptd-server bot block   <userID>         停用某人对 bot 的使用
  yptd-server bot unblock <userID>         恢复
  yptd-server bot list                     谁被停用了
  yptd-server bot check                    自检：opencode 是否可用

配置从环境变量读取：
  YPTD_LISTEN         默认 127.0.0.1:7080
  YPTD_MONGO_URI      必填
  YPTD_OPENIM_API     默认 http://127.0.0.1:10002
  YPTD_OPENIM_SECRET  必填，OpenIM share.yml 里的 secret
  YPTD_OPENIM_ADMIN   默认 imAdmin
  YPTD_INVITE_TTL     默认 24h

bot 相关（不设 YPTD_BOT_OPENCODE_URL 就整个关闭）：
  YPTD_BOT_USER            默认 agentbot
  YPTD_BOT_NICKNAME        默认 助手
  YPTD_BOT_OPENCODE_URL    opencode serve 的地址
  YPTD_BOT_OPENCODE_USER   默认 yptd
  YPTD_BOT_OPENCODE_PASSWORD
  YPTD_BOT_MODEL           默认 zhipuai/glm-5.3
  YPTD_BOT_TIMEOUT         默认 5m
`

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "错误:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		fmt.Print(usage)
		return nil
	}
	switch args[0] {
	case "serve":
		return cmdServe()
	case "invite":
		return cmdInvite(args[1:])
	case "user":
		return cmdUser(args[1:])
	case "bot":
		return cmdBot(args[1:])
	case "check":
		return cmdCheck()
	case "-h", "--help", "help":
		fmt.Print(usage)
		return nil
	default:
		return fmt.Errorf("未知命令 %q，跑 yptd-server --help 看用法", args[0])
	}
}

// open wires config, store and the OpenIM client for any subcommand.
func open(ctx context.Context) (config.Config, *store.Store, *openim.Client, error) {
	cfg, err := config.Load()
	if err != nil {
		return config.Config{}, nil, nil, err
	}
	st, err := store.Open(ctx, cfg.MongoURI, cfg.MongoDB)
	if err != nil {
		return config.Config{}, nil, nil, err
	}
	im := openim.New(cfg.OpenIMAPI, cfg.OpenIMSecret, cfg.OpenIMAdminUserID)
	return cfg, st, im, nil
}

func cmdServe() error {
	ctx := context.Background()
	cfg, st, im, err := open(ctx)
	if err != nil {
		return err
	}
	defer st.Close(context.Background())

	log := slog.New(slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	srv := &http.Server{
		Addr:              cfg.Listen,
		Handler:           api.New(cfg, st, im, log).Routes(),
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      30 * time.Second,
		IdleTimeout:       120 * time.Second,
	}

	// Fail loudly at boot rather than on a user's first login attempt.
	pingCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	if err := im.Ping(pingCtx); err != nil {
		log.Warn("OpenIM 暂时不可用，服务照常启动", "err", err)
	} else {
		log.Info("OpenIM 连通", "api", cfg.OpenIMAPI)
	}

	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	errc := make(chan error, 1)
	go func() {
		log.Info("yptd-server 启动", "listen", cfg.Listen, "db", cfg.MongoDB)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			errc <- err
		}
	}()

	select {
	case err := <-errc:
		return err
	case <-stop:
		log.Info("收到停止信号，优雅关闭")
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		return srv.Shutdown(shutdownCtx)
	}
}

func cmdInvite(args []string) error {
	if len(args) == 0 {
		return errors.New("用法: yptd-server invite new|list|rm")
	}
	ctx := context.Background()
	cfg, st, _, err := open(ctx)
	if err != nil {
		return err
	}
	defer st.Close(ctx)

	switch args[0] {
	case "new":
		count := 1
		var note string
		rest := args[1:]
		for i := 0; i < len(rest); i++ {
			if rest[i] == "-n" && i+1 < len(rest) {
				if _, err := fmt.Sscanf(rest[i+1], "%d", &count); err != nil || count < 1 || count > 50 {
					return errors.New("-n 需要 1..50 之间的数字")
				}
				i++
				continue
			}
			note = strings.TrimSpace(note + " " + rest[i])
		}
		for range count {
			code, err := invite.New()
			if err != nil {
				return err
			}
			inv, err := st.CreateInvite(ctx, code, note, cfg.InviteTTL)
			if err != nil {
				return err
			}
			fmt.Printf("%s   有效期至 %s", inv.Code, inv.ExpiresAt.Local().Format("2006-01-02 15:04"))
			if note != "" {
				fmt.Printf("   （%s）", note)
			}
			fmt.Println()
		}
		return nil

	case "list":
		includeUsed := len(args) > 1 && args[1] == "-a"
		invs, err := st.ListInvites(ctx, includeUsed)
		if err != nil {
			return err
		}
		if len(invs) == 0 {
			fmt.Println("没有邀请码")
			return nil
		}
		tw := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
		fmt.Fprintln(tw, "邀请码\t状态\t有效期至\t备注")
		for _, i := range invs {
			status := "未使用"
			switch {
			case i.Used():
				status = "已用 → " + i.UsedBy
			case time.Now().After(i.ExpiresAt):
				status = "已过期"
			}
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", i.Code, status,
				i.ExpiresAt.Local().Format("01-02 15:04"), i.Note)
		}
		return tw.Flush()

	case "rm":
		if len(args) < 2 {
			return errors.New("用法: yptd-server invite rm <码>")
		}
		code := invite.Normalize(args[1])
		if code == "" {
			return fmt.Errorf("%q 不是合法的邀请码", args[1])
		}
		if err := st.DeleteInvite(ctx, code); err != nil {
			return err
		}
		fmt.Println("已删除", code)
		return nil
	}
	return fmt.Errorf("未知子命令 %q", args[0])
}

func cmdUser(args []string) error {
	if len(args) == 0 {
		return errors.New("用法: yptd-server user list|disable|enable|revoke|rm")
	}
	ctx := context.Background()
	_, st, _, err := open(ctx)
	if err != nil {
		return err
	}
	defer st.Close(ctx)

	switch args[0] {
	case "list":
		users, err := st.ListUsers(ctx)
		if err != nil {
			return err
		}
		if len(users) == 0 {
			fmt.Println("还没有用户")
			return nil
		}
		tw := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
		fmt.Fprintln(tw, "userID\t昵称\t状态\t设备\t注册时间")
		for _, u := range users {
			creds, _ := st.ListCredentials(ctx, u.UserID)
			status := "正常"
			if u.Disabled {
				status = "已停用"
			}
			if u.PasswordHash != "" {
				status += "·有密码"
			}
			fmt.Fprintf(tw, "%s\t%s\t%s\t%d\t%s\n", u.UserID, u.Nickname, status,
				len(creds), u.CreatedAt.Local().Format("01-02 15:04"))
		}
		return tw.Flush()

	case "disable", "enable":
		if len(args) < 2 {
			return fmt.Errorf("用法: yptd-server user %s <userID>", args[0])
		}
		disabled := args[0] == "disable"
		if err := st.SetUserDisabled(ctx, args[1], disabled); err != nil {
			return err
		}
		// Disabling without revoking would leave live sessions running until
		// their OpenIM token expired, which is up to 90 days away.
		if disabled {
			n, err := st.RevokeUserCredentials(ctx, args[1])
			if err != nil {
				return err
			}
			fmt.Printf("已停用 %s，同时吊销 %d 个设备凭据\n", args[1], n)
			return nil
		}
		fmt.Printf("已恢复 %s（需要用邀请码或密码重新登录）\n", args[1])
		return nil

	case "revoke":
		if len(args) < 2 {
			return errors.New("用法: yptd-server user revoke <userID>")
		}
		n, err := st.RevokeUserCredentials(ctx, args[1])
		if err != nil {
			return err
		}
		fmt.Printf("已吊销 %s 的 %d 个设备凭据\n", args[1], n)
		return nil

	case "rm":
		if len(args) < 2 {
			return errors.New("用法: yptd-server user rm <userID>")
		}
		// 先断掉登录，再从花名册拿掉：反过来的话，中间那一刻这个人既看不见
		// 又还能登录。
		if _, err := st.RevokeUserCredentials(ctx, args[1]); err != nil {
			return err
		}
		removed, err := st.DeleteUser(ctx, args[1])
		if err != nil {
			return err
		}
		if !removed {
			fmt.Printf("没有 %s 这个账号\n", args[1])
			return nil
		}
		fmt.Printf("已删除 %s：凭据吊销，不再出现在花名册里\n", args[1])
		fmt.Println("OpenIM 侧的账号删不掉（没有这个接口），但客户端读的是花名册，看不到它。")
		return nil
	}
	return fmt.Errorf("未知子命令 %q", args[0])
}

func cmdCheck() error {
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	cfg, st, im, err := open(ctx)
	if err != nil {
		return err
	}
	defer st.Close(context.Background())

	fmt.Printf("  Mongo   %s  ✓\n", cfg.MongoDB)
	if err := im.Ping(ctx); err != nil {
		fmt.Printf("  OpenIM  %s  ✗ %v\n", cfg.OpenIMAPI, err)
		return errors.New("自检未通过")
	}
	fmt.Printf("  OpenIM  %s  ✓\n", cfg.OpenIMAPI)
	users, err := st.ListUsers(ctx)
	if err != nil {
		return err
	}
	invs, err := st.ListInvites(ctx, false)
	if err != nil {
		return err
	}
	fmt.Printf("  用户 %d 个，未使用邀请码 %d 个\n", len(users), len(invs))
	return nil
}

// ------------------------------------------------------------------- bot ---

func cmdBot(args []string) error {
	if len(args) == 0 {
		return errors.New("用法: yptd-server bot setup|block|unblock|list|check")
	}
	ctx := context.Background()
	cfg, st, im, err := open(ctx)
	if err != nil {
		return err
	}
	defer st.Close(ctx)

	switch args[0] {
	case "setup":
		exists, err := im.UserExists(ctx, cfg.BotUserID)
		if err != nil {
			return err
		}
		if exists {
			// 改过 YPTD_BOT_NICKNAME 的话，OpenIM 那边还留着旧名字，
			// 群成员列表和私聊标题都会继续显示旧的。
			if err := im.UpdateUser(ctx, cfg.BotUserID, cfg.BotNickname); err != nil {
				return err
			}
		} else if err := im.RegisterUser(ctx, cfg.BotUserID, cfg.BotNickname, ""); err != nil {
			return err
		}
		// A local row too, so `user list` shows it and nobody wonders where
		// this account came from. The roster the clients read is this row,
		// not OpenIM's copy, so a rename has to land in both.
		if row, err := st.GetUser(ctx, cfg.BotUserID); err != nil {
			_ = st.CreateUser(ctx, store.User{
				UserID:    cfg.BotUserID,
				Nickname:  cfg.BotNickname,
				CreatedAt: time.Now(),
			})
		} else if row.Nickname != cfg.BotNickname {
			if err := st.SetUserNickname(ctx, cfg.BotUserID, cfg.BotNickname); err != nil {
				return err
			}
		}
		fmt.Printf("bot 账号就绪: %s（%s）\n", cfg.BotUserID, cfg.BotNickname)
		fmt.Println("把它拉进群，然后 @ 它提问。有账号的人都能用，不用另外开权限。")
		return nil

	case "block":
		if len(args) < 2 {
			return errors.New("用法: yptd-server bot block <userID>")
		}
		if err := st.BotBlock(ctx, args[1]); err != nil {
			return err
		}
		fmt.Printf("%s 不能再使唤 bot 了\n", args[1])
		return nil

	case "unblock":
		if len(args) < 2 {
			return errors.New("用法: yptd-server bot unblock <userID>")
		}
		removed, err := st.BotUnblock(ctx, args[1])
		if err != nil {
			return err
		}
		if !removed {
			fmt.Printf("%s 本来就没被停用\n", args[1])
			return nil
		}
		fmt.Printf("已恢复 %s\n", args[1])
		return nil

	case "list":
		ids, err := st.BotBlockList(ctx)
		if err != nil {
			return err
		}
		if len(ids) == 0 {
			fmt.Println("没有人被停用：有 yptd 账号的人都能使唤 bot。")
			fmt.Println("注册要邀请码，那一道就是门槛。")
			return nil
		}
		for _, id := range ids {
			fmt.Println(" ", id)
		}
		return nil

	case "check":
		if !cfg.BotEnabled() {
			return errors.New("没配 YPTD_BOT_OPENCODE_URL，bot 是关着的")
		}
		agent := bot.NewOpencode(cfg.BotOpencodeURL, cfg.BotOpencodeUser,
			cfg.BotOpencodePassword, cfg.BotModel, 30*time.Second)
		version, err := agent.Health(ctx)
		if err != nil {
			return fmt.Errorf("opencode 不可用: %w", err)
		}
		fmt.Printf("opencode %s 在 %s，模型 %s\n", version, cfg.BotOpencodeURL, cfg.BotModel)
		exists, err := im.UserExists(ctx, cfg.BotUserID)
		if err != nil {
			return err
		}
		fmt.Printf("bot 账号 %s: %s\n", cfg.BotUserID, map[bool]string{true: "已存在", false: "还没建，跑 bot setup"}[exists])
		ids, err := st.BotBlockList(ctx)
		if err != nil {
			return err
		}
		fmt.Printf("停用 %d 人（其余有账号的都能用）\n", len(ids))
		return nil
	}
	return fmt.Errorf("未知子命令 bot %s", args[0])
}
