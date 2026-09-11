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
const placeholderText = "⏳ 正在处理…"

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
	return b
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
	case strings.TrimSpace(m.Text) == "":
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
	} else if id, err := b.say(ctx, m, placeholderText, pendingEx(r.ID())); err == nil {
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
