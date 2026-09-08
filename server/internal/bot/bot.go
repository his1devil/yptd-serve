package bot

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"
)

// Message is one incoming message, already stripped of OpenIM's shapes.
type Message struct {
	SenderID       string
	SenderNickname string
	GroupID        string
	// ConversationID is what a revoke needs: `sg_<group>` or `si_<a>_<b>`.
	ConversationID string
	ContentType    int32
	// Text is what the person actually typed, with the leading @mention of
	// the bot removed.
	Text string
	// Mentioned is true when this message was addressed to the bot.
	Mentioned bool
}

// Sender is the slice of OpenIM this package needs. An interface so the
// decision logic can be tested without a server.
type Sender interface {
	SendText(ctx context.Context, sender, nickname, recvID, groupID, text, ex string) error
}

// Sessions remembers which opencode conversation belongs to which chat.
type Sessions interface {
	BotSession(ctx context.Context, conversationID string) (string, error)
	SetBotSession(ctx context.Context, conversationID, sessionID string) error
	BotAllowed(ctx context.Context, userID string) (bool, error)
}

// How long to wait for an answer before saying anything, and what to say.
const (
	placeholderAfter = 3 * time.Second
	placeholderText  = "⏳ 正在处理…"
	// Marks a message its sender means to replace.
	pendingMarker = `{"yptd":"pending"}`
)

type Config struct {
	UserID   string
	Nickname string
	// Timeout bounds one question. A coding agent left alone can grind for a
	// very long time, and a group chat is not the place to find out.
	Timeout time.Duration
	// MaxConcurrent caps how many questions run at once across all chats.
	MaxConcurrent int
}

// Bot turns messages into answers.
type Bot struct {
	cfg   Config
	im    Sender
	store Sessions
	agent *Opencode
	log   *slog.Logger
	slots chan struct{}
	mu    sync.Mutex
	// busy holds one lock per conversation, so two questions in the same
	// group queue instead of interleaving their answers.
	busy map[string]*sync.Mutex
}

func New(cfg Config, im Sender, store Sessions, agent *Opencode, log *slog.Logger) *Bot {
	if cfg.Timeout <= 0 {
		cfg.Timeout = 5 * time.Minute
	}
	if cfg.MaxConcurrent <= 0 {
		cfg.MaxConcurrent = 2
	}
	return &Bot{
		cfg:   cfg,
		im:    im,
		store: store,
		agent: agent,
		log:   log,
		slots: make(chan struct{}, cfg.MaxConcurrent),
		busy:  map[string]*sync.Mutex{},
	}
}

// Wants reports whether this message is the bot's to answer.
//
// Deliberately strict. The webhook fires for every message in every group,
// and answering one that was not addressed to the bot is worse than missing
// one that was.
func (b *Bot) Wants(m Message) bool {
	switch {
	case m.SenderID == b.cfg.UserID:
		// Its own replies come back through the same webhook. Answering them
		// is an infinite loop that costs money.
		return false
	case !m.Mentioned:
		return false
	case m.ContentType != 101 && m.ContentType != 106:
		return false
	case strings.TrimSpace(m.Text) == "":
		return false
	}
	return true
}

// Handle answers one message. It blocks, so callers on a webhook thread must
// run it in a goroutine: OpenIM gives the callback five seconds.
func (b *Bot) Handle(ctx context.Context, m Message) {
	allowed, err := b.store.BotAllowed(ctx, m.SenderID)
	if err != nil {
		b.log.Error("bot: whitelist lookup", "err", err)
		return
	}
	if !allowed {
		b.log.Info("bot: sender not allowed", "user", m.SenderID)
		b.say(ctx, m, fmt.Sprintf("@%s 你还没有被允许使用这个助手。让管理员执行 yptd-server bot allow %s", m.SenderNickname, m.SenderID), false)
		return
	}

	lock := b.lockFor(m.ConversationID)
	lock.Lock()
	defer lock.Unlock()

	select {
	case b.slots <- struct{}{}:
		defer func() { <-b.slots }()
	case <-ctx.Done():
		return
	}

	// Answer first, placeholder only if the answer is slow. A reasoning
	// model usually takes tens of seconds, and silence looks like a broken
	// bot -- but a quick answer should not be preceded by noise.
	type result struct {
		text string
		err  error
	}
	done := make(chan result, 1)
	go func() {
		text, err := b.ask(ctx, m)
		done <- result{text, err}
	}()

	var answer string
	select {
	case r := <-done:
		answer, err = r.text, r.err
	case <-time.After(placeholderAfter):
		b.say(ctx, m, placeholderText, true)
		r := <-done
		answer, err = r.text, r.err
	}

	if err != nil {
		b.log.Error("bot: ask", "err", err, "conversation", m.ConversationID)
		answer = "我这边出错了：" + oneLine(err.Error())
	}
	if strings.TrimSpace(answer) == "" {
		answer = "（模型没有给出内容，可以换个说法再问一次）"
	}
	b.say(ctx, m, answer, false)
}

func (b *Bot) ask(ctx context.Context, m Message) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, b.cfg.Timeout)
	defer cancel()

	session, err := b.store.BotSession(ctx, m.ConversationID)
	if err != nil {
		return "", err
	}
	if session == "" {
		session, err = b.agent.NewSession(ctx, "yptd "+m.ConversationID)
		if err != nil {
			return "", err
		}
		if err := b.store.SetBotSession(ctx, m.ConversationID, session); err != nil {
			return "", err
		}
	}
	prompt := fmt.Sprintf("%s 在群聊里问你：%s", m.SenderNickname, m.Text)
	answer, err := b.agent.Ask(ctx, session, prompt)
	if err == nil {
		return answer, nil
	}
	// A session the agent has forgotten (restarted, database cleared) must
	// not wedge this conversation forever.
	fresh, newErr := b.agent.NewSession(ctx, "yptd "+m.ConversationID)
	if newErr != nil {
		return "", err
	}
	if err := b.store.SetBotSession(ctx, m.ConversationID, fresh); err != nil {
		return "", err
	}
	return b.agent.Ask(ctx, fresh, prompt)
}

func (b *Bot) say(ctx context.Context, m Message, text string, pending bool) {
	recv := ""
	if m.GroupID == "" {
		recv = m.SenderID
	}
	// The marker rides in OpenIM's free-form `ex`, which lets a client hide
	// the placeholder once the answer lands. Withdrawing it instead is not
	// available: a revoke is addressed by sequence number, and the number of
	// a message is not readable until after it has propagated, so a revoke
	// fired straight after sending takes back whatever came before it.
	ex := ""
	if pending {
		ex = pendingMarker
	}
	if err := b.im.SendText(ctx, b.cfg.UserID, b.cfg.Nickname, recv, m.GroupID, text, ex); err != nil {
		b.log.Error("bot: send", "err", err, "conversation", m.ConversationID)
	}
}

func (b *Bot) lockFor(conversationID string) *sync.Mutex {
	b.mu.Lock()
	defer b.mu.Unlock()
	lock, ok := b.busy[conversationID]
	if !ok {
		lock = &sync.Mutex{}
		b.busy[conversationID] = lock
	}
	return lock
}

func oneLine(value string) string {
	value = strings.TrimSpace(strings.ReplaceAll(value, "\n", " "))
	if len(value) > 300 {
		return value[:300] + "…"
	}
	return value
}

// ParseContent pulls the typed text out of OpenIM's content JSON and strips
// the mention that addressed the bot, so the model is asked the question
// rather than the message.
func ParseContent(contentType int32, content, botNickname string) (string, []string) {
	var mentions []string
	text := ""
	switch contentType {
	case 101:
		var elem struct {
			Content string `json:"content"`
		}
		_ = json.Unmarshal([]byte(content), &elem)
		text = elem.Content
	case 106:
		var elem struct {
			Text        string   `json:"text"`
			AtUserList  []string `json:"atUserList"`
			AtUsersInfo []struct {
				AtUserID      string `json:"atUserID"`
				GroupNickname string `json:"groupNickname"`
			} `json:"atUsersInfo"`
		}
		_ = json.Unmarshal([]byte(content), &elem)
		text = elem.Text
		mentions = elem.AtUserList
		for _, info := range elem.AtUsersInfo {
			text = strings.ReplaceAll(text, "@"+info.GroupNickname, "")
		}
	}
	if botNickname != "" {
		text = strings.ReplaceAll(text, "@"+botNickname, "")
	}
	return strings.TrimSpace(text), mentions
}
