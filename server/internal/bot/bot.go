package bot

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/his1devil/yptd/server/internal/config"
	"github.com/his1devil/yptd/server/internal/run"
)

// Message is one incoming message, already stripped of OpenIM's shapes.
type Message struct {
	// ClientMsgID is what a reaction to this message is addressed by.
	ClientMsgID    string
	SenderID       string
	SenderNickname string
	GroupID        string
	// ConversationID is what a revoke needs: `sg_<group>` or `si_<a>_<b>`.
	ConversationID string
	ContentType    int32
	// Text is what the person actually typed, with the @mentions of the
	// agents removed.
	Text string
	// AgentID is the agent this message was addressed to, empty when it was
	// addressed to none. One message mentioning two agents becomes two
	// messages, one per agent.
	AgentID string
	// Attachments are the files sent alongside the text, from yptd's `ex`
	// extension on the message. The model gets their links.
	Attachments []Attachment
}

// Attachment is one file a desktop client put in a message next to its text:
// `ex` = {"yptd":"rich","a":[{"k":"i"|"f","u":url,"n":name,"s":bytes,...}]}.
type Attachment struct {
	Kind string // "image", "video" or "file"
	URL  string
	Name string
}

// ParseAttachments reads the attachment list out of a message's `ex`; nil
// when there is none or it is some other client's extension.
func ParseAttachments(ex string) []Attachment {
	a, _ := parseRich(ex)
	return a
}

// Textless reports whether the message carried attachments and no typed text.
// The client fills the text with "[图片]" so older clients show something;
// that placeholder is not what the sender said and should not reach the model.
func Textless(ex string) bool {
	_, textless := parseRich(ex)
	return textless
}

func parseRich(ex string) ([]Attachment, bool) {
	if ex == "" {
		return nil, false
	}
	var p struct {
		Yptd string `json:"yptd"`
		T    *int   `json:"t"`
		A    []struct {
			K string `json:"k"`
			U string `json:"u"`
			N string `json:"n"`
		} `json:"a"`
	}
	if json.Unmarshal([]byte(ex), &p) != nil || p.Yptd != "rich" {
		return nil, false
	}
	out := make([]Attachment, 0, len(p.A))
	for _, a := range p.A {
		if a.U == "" {
			continue
		}
		kind := "file"
		switch a.K {
		case "i":
			kind = "image"
		case "v":
			// 视频以前落进「文件」，模型拿到的是「文件 1EF68611-….mp4」——iOS 用临时文件的
			// UUID 当名字，光看这个它不知道那是一段视频。
			kind = "video"
		}
		out = append(out, Attachment{Kind: kind, URL: a.U, Name: a.N})
	}
	return out, p.T != nil && *p.T == 0 && len(out) > 0
}

// Sender is the slice of OpenIM this package needs. An interface so the
// decision logic can be tested without a server. SendText returns the new
// message's clientMsgID.
type Sender interface {
	SendText(ctx context.Context, sender, nickname, recvID, groupID, text, ex string) (string, error)
	// SendCustom posts a custom message -- in yptd's protocol, a reaction.
	SendCustom(ctx context.Context, sender, nickname, recvID, groupID, data, description string) (string, error)
}

// Sessions remembers which opencode conversation belongs to which chat.
//
// Keyed by agent as well as by chat: two agents in one group are two separate
// conversations, and threading them together would let each read the other's
// context as if it were its own.
type Sessions interface {
	BotSession(ctx context.Context, agentID, conversationID string) (string, error)
	SetBotSession(ctx context.Context, agentID, conversationID, sessionID string) error
	BotBlocked(ctx context.Context, userID string) (bool, error)
}

// In a direct chat the placeholder goes out the moment a run starts. It
// carries the run id, so a client that knows about runs replaces it with the
// live answer as it streams; one that does not shows the text and hides it
// when the answer lands.
// 导出是给 beforeOfflinePush 回调认的：这条不该推到手机上。
const PlaceholderText = "⏳ 正在处理…"

// In a group the agent reacts to the question instead. A placeholder there
// makes the stream jump around while everyone else keeps talking; a reaction
// costs the rendering nothing and still says "seen, on it". The full answer,
// with the thinking and tool calls folded up behind it, lands once.
const ackEmoji = "👌"

// reactionData is the payload of a reaction in yptd's client protocol.
func reactionData(target, emoji string) string {
	raw, _ := json.Marshal(map[string]string{"yptd": "reaction", "target": target, "emoji": emoji})
	return string(raw)
}

// ErrNoRun is Cancel's answer for an id this process does not know.
var ErrNoRun = errors.New("bot: no such run")

// Markers ride in OpenIM's free-form `ex`.
func pendingEx(runID string) string { return fmt.Sprintf(`{"yptd":"pending","run":%q}`, runID) }
func runEx(runID string) string     { return fmt.Sprintf(`{"yptd":"run","run":%q}`, runID) }

type Config struct {
	// Agents are the accounts this bot answers as, in roster order.
	Agents []config.Agent
	// Timeout bounds one question. A coding agent left alone can grind for a
	// very long time, and a group chat is not the place to find out.
	Timeout time.Duration
	// MaxConcurrent caps how many questions run at once across all chats.
	MaxConcurrent int
	// Quotes backs the market watch. Nil disables every agent's watch, which
	// is the right default for a deployment without the Longbridge CLI.
	Quotes Quotes
	// Groups answers where an agent can speak, for the watch and the news
	// push. Nil disables both for the same reason.
	Groups Groups
	// Feed builds the news reader an agent asks for by name. Nil disables
	// every agent's news push.
	Feed FeedMaker
	// Rooms answers what each agent should be doing in each group, read fresh
	// every round so a change made in the app takes effect without a restart.
	Rooms Rooms
}

// Bot turns messages into answers, as whichever agent was addressed.
type Bot struct {
	cfg   Config
	byID  map[string]config.Agent
	im    Sender
	store Sessions
	agent *Opencode
	runs  *run.Registry
	log   *slog.Logger
	slots chan struct{}
	mu    sync.Mutex
	// busy holds one lock per conversation, so two questions in the same
	// group queue instead of interleaving their answers. Deliberately per
	// conversation and not per agent: two agents answering the same group at
	// once would talk over each other.
	busy map[string]*sync.Mutex
	// streams is the per-run reducer state for opencode's events.
	smu     sync.Mutex
	streams map[string]*stream
	// descriptions caches opencode's agent summaries; they change only when
	// somebody edits an agent file on the server.
	dmu     sync.Mutex
	descs   map[string]string
	descsAt time.Time
}

func New(cfg Config, im Sender, store Sessions, agent *Opencode, runs *run.Registry, log *slog.Logger) *Bot {
	if cfg.Timeout <= 0 {
		cfg.Timeout = 5 * time.Minute
	}
	if cfg.MaxConcurrent <= 0 {
		cfg.MaxConcurrent = 2
	}
	if runs == nil {
		runs = run.NewRegistry(0)
	}
	byID := make(map[string]config.Agent, len(cfg.Agents))
	for _, a := range cfg.Agents {
		byID[a.UserID] = a
	}
	b := &Bot{
		cfg:     cfg,
		byID:    byID,
		im:      im,
		store:   store,
		agent:   agent,
		runs:    runs,
		log:     log,
		slots:   make(chan struct{}, cfg.MaxConcurrent),
		busy:    map[string]*sync.Mutex{},
		streams: map[string]*stream{},
	}
	if agent != nil {
		go b.eventLoop(context.Background())
	}
	b.startWatches(context.Background())
	b.startNews(context.Background())
	return b
}

// startWatches launches one poller per agent that has a watchlist. Separate
// from the answering path on purpose: a watch neither takes a concurrency
// slot nor touches the opencode session, so a busy room cannot delay an
// alert and an alert cannot pollute a room's agent context.
func (b *Bot) startWatches(ctx context.Context) {
	if b.cfg.Quotes == nil || b.cfg.Groups == nil || b.cfg.Rooms == nil {
		return
	}
	for _, a := range b.cfg.Agents {
		// 每个 agent 都起一个：清单现在按群配在库里，启动时 agents.json 里空着
		// 不代表以后也空着——watcher 每轮都会重读。
		go NewWatcher(a, b.cfg.Rooms, b.cfg.Quotes, b.im, b.cfg.Groups, b.log).Run(ctx)
	}
}

// startNews launches one poller per agent that has a news feed. Each gets its
// own reader: the feed carries a cursor, and a shared one would let whichever
// agent polled first eat the other's changes.
func (b *Bot) startNews(ctx context.Context) {
	if b.cfg.Feed == nil || b.cfg.Groups == nil || b.agent == nil {
		return
	}
	for _, a := range b.cfg.Agents {
		if !a.News.On() {
			continue
		}
		feed := b.cfg.Feed(a.News.Feed)
		if feed == nil {
			b.log.Warn("news: unknown feed", "agent", a.UserID, "feed", a.News.Feed)
			continue
		}
		go NewNewsWatcher(a, feed, b.agent, b.im, b.cfg.Groups, b.log).Run(ctx)
	}
}

// Runs is the registry, for the HTTP layer to serve from.
func (b *Bot) Runs() *run.Registry { return b.runs }

// Describe returns each roster agent's one-line persona, keyed by user id,
// as opencode describes the agent it routes to. Cached for a minute.
func (b *Bot) Describe(ctx context.Context) map[string]string {
	b.dmu.Lock()
	defer b.dmu.Unlock()
	if b.descs != nil && time.Since(b.descsAt) < time.Minute {
		return b.descs
	}
	out := map[string]string{}
	if b.agent != nil {
		if infos, err := b.agent.Agents(ctx); err == nil {
			byName := make(map[string]string, len(infos))
			for _, a := range infos {
				byName[a.Name] = a.Description
			}
			for _, a := range b.cfg.Agents {
				if d, ok := byName[a.Opencode]; ok {
					out[a.UserID] = d
				}
			}
		} else {
			b.log.Warn("bot: describe agents", "err", err)
		}
	}
	b.descs, b.descsAt = out, time.Now()
	return out
}

// Agents is the roster, for the callback's mention matching and the CLI.
func (b *Bot) Agents() []config.Agent { return b.cfg.Agents }

// IsAgent reports whether this account is one of ours.
func (b *Bot) IsAgent(userID string) bool {
	_, ok := b.byID[userID]
	return ok
}

// Nicknames is every agent's display name, for stripping mentions out of the
// text before it reaches the model.
func (b *Bot) Nicknames() []string {
	out := make([]string, 0, len(b.cfg.Agents))
	for _, a := range b.cfg.Agents {
		out = append(out, a.Nickname)
	}
	return out
}

// Wants reports whether this message is the bot's to answer.
//
// Deliberately strict. The webhook fires for every message in every group,
// and answering one that was not addressed to the bot is worse than missing
// one that was.
func (b *Bot) Wants(m Message) bool {
	switch {
	case b.IsAgent(m.SenderID):
		// Their own replies come back through the same webhook, and one agent
		// answering another is an infinite loop that costs money.
		return false
	case m.AgentID == "" || !b.IsAgent(m.AgentID):
		return false
	case m.ContentType != 101 && m.ContentType != 106:
		return false
	case strings.TrimSpace(m.Text) == "" && len(m.Attachments) == 0:
		// 「@Dummy」加一张截图，去掉 @ 之后文字是空的，但截图就是内容。
		// 真正的空消息是既没字也没附件。
		return false
	}
	return true
}

// Handle answers one message. It blocks, so callers on a webhook thread must
// run it in a goroutine: OpenIM gives the callback five seconds.
func (b *Bot) Handle(ctx context.Context, m Message) {
	// Anyone with an account may ask: registering already needs an invitation
	// code, so that is the gate. This list is only the people it was taken
	// back from.
	blocked, err := b.store.BotBlocked(ctx, m.SenderID)
	if err != nil {
		b.log.Error("bot: block list lookup", "err", err)
		return
	}
	if blocked {
		b.log.Info("bot: sender blocked", "user", m.SenderID)
		b.say(ctx, m, fmt.Sprintf("@%s 管理员停用了你对这个助手的使用。", m.SenderNickname), "")
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

	text, r := b.answer(ctx, m)
	id, err := b.say(ctx, m, text, runEx(r.ID()))
	if err != nil {
		b.log.Error("bot: send answer", "err", err, "conversation", m.ConversationID, "run", r.ID())
	}
	sum := b.runs.Finish(r, id)
	b.forget(r)
	b.log.Info("bot: run finished", "run", sum.ID, "agent", sum.AgentID, "status", sum.Status,
		"ms", sum.EndedAt-sum.StartedAt, "steps", sum.Usage.Steps, "tools", len(sum.Tools), "chars", len(sum.Text))
}

// answer runs one question through opencode and returns what to post. The
// run it returns is still open; the caller finishes it once the answer has a
// message id.
func (b *Bot) answer(ctx context.Context, m Message) (string, *run.Run) {
	ctx, cancel := context.WithTimeout(ctx, b.cfg.Timeout)
	defer cancel()

	who := b.byID[m.AgentID]
	title := fmt.Sprintf("yptd %s %s", who.UserID, m.ConversationID)
	where := "在群聊里问你"
	if m.GroupID == "" {
		where = "问你"
	}
	prompt := fmt.Sprintf("%s %s：%s", m.SenderNickname, where, m.Text)
	if strings.TrimSpace(m.Text) == "" {
		// 只发了附件。别让模型对着一个冒号猜问题是什么。
		prompt = fmt.Sprintf("%s %s发来了附件，没有写文字。", m.SenderNickname, strings.TrimSuffix(where, "问你"))
	}
	if len(m.Attachments) > 0 {
		// Links rather than bytes: the model can fetch what it needs, and a
		// screenshot pasted next to a question should not be invisible to it.
		prompt += "\n\n对方随消息附了文件（可以用工具读取链接）："
		for _, a := range m.Attachments {
			label := "文件"
			switch a.Kind {
			case "image":
				label = "图片"
			case "video":
				label = "视频"
			}
			prompt += fmt.Sprintf("\n- %s %s：%s", label, a.Name, a.URL)
		}
	}
	// 账号、群号、消息 id 放在最后一行：Dummy 记 bug 要写「谁 (账号)」「#哪个群」和
	// 能跳回原消息的 id，只给昵称它填不出来。别的 agent 多半用不上，一行也不碍事。
	prompt += fmt.Sprintf("\n\n（发送者账号 %s", m.SenderID)
	if m.GroupID != "" {
		prompt += fmt.Sprintf("，群 %s", m.GroupID)
	}
	if m.ClientMsgID != "" {
		prompt += fmt.Sprintf("，消息 id %s", m.ClientMsgID)
	}
	prompt += "）"

	r := b.runs.Start(run.Summary{
		AgentID: who.UserID, ConversationID: m.ConversationID, RequesterID: m.SenderID, Prompt: m.Text,
	}, "")
	b.remember(r, newStream(prompt))

	if m.GroupID != "" {
		if m.ClientMsgID != "" {
			if _, err := b.im.SendCustom(ctx, who.UserID, who.Nickname, "", m.GroupID, reactionData(m.ClientMsgID, ackEmoji), "reaction"); err != nil {
				b.log.Warn("bot: ack reaction", "err", err, "conversation", m.ConversationID)
			}
		}
	} else if id, err := b.say(ctx, m, PlaceholderText, pendingEx(r.ID())); err == nil {
		// The placeholder goes out first and at once: it is how a client learns
		// the run id, and every second of silence before it looks like a broken bot.
		r.SetPlaceholder(id)
	}

	fail := func(err error) (string, *run.Run) {
		b.log.Error("bot: ask", "err", err, "conversation", m.ConversationID, "run", r.ID())
		r.Fail(err.Error())
		return "我这边出错了：" + oneLine(err.Error()), r
	}

	session, err := b.store.BotSession(ctx, who.UserID, m.ConversationID)
	if err != nil {
		return fail(err)
	}
	if session == "" {
		if session, err = b.newSession(ctx, who, m.ConversationID, title); err != nil {
			return fail(err)
		}
	}
	// Bind before prompting: the first events can arrive before the request
	// has even returned, and events for an unbound session are dropped.
	b.runs.Rebind(r, session)
	if err := b.agent.PromptAsync(ctx, session, who.Opencode, who.Model, prompt); err != nil {
		// A session the agent has forgotten (restarted, database cleared) must
		// not wedge this conversation forever.
		fresh, newErr := b.newSession(ctx, who, m.ConversationID, title)
		if newErr != nil {
			return fail(err)
		}
		session = fresh
		b.runs.Rebind(r, session)
		if err := b.agent.PromptAsync(ctx, session, who.Opencode, who.Model, prompt); err != nil {
			return fail(err)
		}
	}

	b.waitIdle(ctx, r, session)

	snap := r.Snapshot()
	text := strings.TrimSpace(snap.Text)
	switch snap.Status {
	case run.StatusError:
		return "我这边出错了：" + oneLine(snap.Error), r
	case run.StatusCancelled:
		if text == "" {
			return "（已停止）", r
		}
		return text + "\n\n（已停止）", r
	}
	if text == "" {
		// The stream may have been down for the whole answer; the answer is
		// still in opencode.
		if got, err := b.agent.LastAnswerSince(ctx, session, snap.StartedAt); err == nil {
			text = strings.TrimSpace(got)
			if text != "" {
				r.Text(text)
			}
		}
	}
	if text == "" {
		text = "（模型没有给出内容，可以换个说法再问一次）"
	}
	return text, r
}

func (b *Bot) newSession(ctx context.Context, who config.Agent, conversationID, title string) (string, error) {
	session, err := b.agent.NewSession(ctx, title)
	if err != nil {
		return "", err
	}
	if err := b.store.SetBotSession(ctx, who.UserID, conversationID, session); err != nil {
		return "", err
	}
	return session, nil
}

// waitIdle blocks until the model has stopped. The event stream normally says
// so; if it was down at the wrong moment, a periodic status check catches the
// end instead of waiting out the whole timeout.
func (b *Bot) waitIdle(ctx context.Context, r *run.Run, session string) {
	poll := time.NewTicker(10 * time.Second)
	defer poll.Stop()
	started := time.Now()
	for {
		select {
		case <-r.Idle():
			return
		case <-ctx.Done():
			// Out of time. Stop the model too, or it keeps spending in the dark.
			actx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			_ = b.agent.Abort(actx, session)
			cancel()
			r.Fail("等了太久，已经停止")
			return
		case <-poll.C:
			if time.Since(started) < 15*time.Second {
				continue
			}
			if busy, err := b.agent.Busy(ctx, session); err == nil && !busy {
				r.MarkIdle()
				return
			}
		}
	}
}

// Cancel stops a run somebody asked to stop. The answer so far is still posted.
func (b *Bot) Cancel(ctx context.Context, runID string) error {
	r, ok := b.runs.Get(runID)
	if !ok {
		return ErrNoRun
	}
	if sid := r.SessionID(); sid != "" {
		if err := b.agent.Abort(ctx, sid); err != nil {
			b.log.Warn("bot: abort", "err", err, "run", runID)
		}
	}
	r.Cancel()
	return nil
}

// eventLoop keeps one subscription to opencode's event stream open and
// routes each event to the run bound to its session.
func (b *Bot) eventLoop(ctx context.Context) {
	backoff := time.Second
	for {
		started := time.Now()
		err := b.agent.Events(ctx, func(ev Event) {
			sid := sessionOf(ev.Properties)
			if sid == "" {
				return
			}
			r, ok := b.runs.BySession(sid)
			if !ok {
				return
			}
			if st := b.streamFor(r); st != nil {
				st.apply(r, ev)
			}
		})
		if ctx.Err() != nil {
			return
		}
		if time.Since(started) > time.Minute {
			backoff = time.Second
		}
		b.log.Warn("bot: opencode event stream dropped, reconnecting", "err", err, "in", backoff)
		time.Sleep(backoff)
		if backoff < 30*time.Second {
			backoff *= 2
		}
	}
}

func sessionOf(props json.RawMessage) string {
	var p struct {
		SessionID string `json:"sessionID"`
	}
	_ = json.Unmarshal(props, &p)
	return p.SessionID
}

func (b *Bot) remember(r *run.Run, st *stream) {
	b.smu.Lock()
	defer b.smu.Unlock()
	b.streams[r.ID()] = st
}

func (b *Bot) streamFor(r *run.Run) *stream {
	b.smu.Lock()
	defer b.smu.Unlock()
	return b.streams[r.ID()]
}

func (b *Bot) forget(r *run.Run) {
	b.smu.Lock()
	defer b.smu.Unlock()
	delete(b.streams, r.ID())
}

func (b *Bot) say(ctx context.Context, m Message, text, ex string) (string, error) {
	who := b.byID[m.AgentID]
	recv := ""
	if m.GroupID == "" {
		recv = m.SenderID
	}
	id, err := b.im.SendText(ctx, who.UserID, who.Nickname, recv, m.GroupID, text, ex)
	if err != nil {
		b.log.Error("bot: send", "err", err, "conversation", m.ConversationID, "agent", who.UserID)
	}
	return id, err
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

// oneLine flattens text onto a single line and caps it. The cap counts runes,
// not bytes: slicing a Chinese string at byte 300 lands in the middle of a
// character and prints a replacement glyph.
func oneLine(value string) string {
	value = strings.Join(strings.Fields(strings.ReplaceAll(value, "\n", " ")), " ")
	if r := []rune(value); len(r) > 300 {
		return string(r[:300]) + "…"
	}
	return value
}

// ParseContent pulls the typed text out of OpenIM's content JSON and strips
// the mention that addressed the bot, so the model is asked the question
// rather than the message.
func ParseContent(contentType int32, content string, botNicknames []string) (string, []string) {
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
	for _, nickname := range botNicknames {
		if nickname != "" {
			text = strings.ReplaceAll(text, "@"+nickname, "")
		}
	}
	return strings.TrimSpace(text), mentions
}
