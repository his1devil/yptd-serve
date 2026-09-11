// Package run keeps the live record of one agent answering one question.
//
// A run is an append-only log of typed events -- thinking and answer text as
// they stream in, each tool call as its state changes, usage per step, and
// finally the message the answer landed in. Everything a client shows is
// derived from that log: a viewer arriving late gets a snapshot of the whole
// current value and then the tail of live events, so it converges on the same
// picture as one that watched from the start. The log lives in memory while
// the run is active and for a while after; the snapshot is what gets
// persisted.
package run

import (
	"crypto/rand"
	"encoding/base32"
	"encoding/json"
	"strings"
	"sync"
	"time"
)

const (
	StatusRunning   = "running"
	StatusDone      = "done"
	StatusError     = "error"
	StatusCancelled = "cancelled"
)

// Event is one appended fact. Seq is contiguous per run, starting at 1.
type Event struct {
	Seq  int             `json:"seq"`
	Type string          `json:"type"`
	At   int64           `json:"at"`
	Data json.RawMessage `json:"data,omitempty"`
}

// Tool is one tool call, upserted by CallID as opencode reports its state.
type Tool struct {
	CallID    string `json:"call_id" bson:"call_id"`
	Name      string `json:"name" bson:"name"`
	Status    string `json:"status" bson:"status"` // pending | running | completed | error
	Title     string `json:"title,omitempty" bson:"title,omitempty"`
	Input     string `json:"input,omitempty" bson:"input,omitempty"`   // JSON text, truncated
	Output    string `json:"output,omitempty" bson:"output,omitempty"` // truncated
	Error     string `json:"error,omitempty" bson:"error,omitempty"`
	StartedAt int64  `json:"started_at,omitempty" bson:"started_at,omitempty"`
	EndedAt   int64  `json:"ended_at,omitempty" bson:"ended_at,omitempty"`
}

type Usage struct {
	Input     int64   `json:"input" bson:"input"`
	Output    int64   `json:"output" bson:"output"`
	Reasoning int64   `json:"reasoning" bson:"reasoning"`
	Cost      float64 `json:"cost" bson:"cost"`
	Steps     int     `json:"steps" bson:"steps"`
}

// Summary is the whole current value of a run: what a late viewer needs to
// draw it, and what is persisted once it ends.
type Summary struct {
	ID               string `json:"id" bson:"_id"`
	AgentID          string `json:"agent_id" bson:"agent_id"`
	ConversationID   string `json:"conversation_id" bson:"conversation_id"`
	RequesterID      string `json:"requester_id" bson:"requester_id"`
	Prompt           string `json:"prompt" bson:"prompt"`
	Status           string `json:"status" bson:"status"`
	StartedAt        int64  `json:"started_at" bson:"started_at"`
	EndedAt          int64  `json:"ended_at,omitempty" bson:"ended_at,omitempty"`
	Thinking         string `json:"thinking" bson:"thinking"`
	Text             string `json:"text" bson:"text"`
	Tools            []Tool `json:"tools" bson:"tools"`
	Usage            Usage  `json:"usage" bson:"usage"`
	Error            string `json:"error,omitempty" bson:"error,omitempty"`
	PlaceholderMsgID string `json:"placeholder_msg_id,omitempty" bson:"placeholder_msg_id,omitempty"`
	FinalMsgID       string `json:"final_msg_id,omitempty" bson:"final_msg_id,omitempty"`
	// Seq is the last event folded into this snapshot. A viewer that has it
	// needs only events after it.
	Seq int `json:"seq" bson:"seq"`
}

// subBuffer is how far a slow viewer may fall behind before it is dropped.
// A dropped viewer reconnects and gets a fresh snapshot, which is cheaper than
// letting one stalled connection hold every delta in memory.
const subBuffer = 512

// Run is the live log plus its subscribers.
type Run struct {
	mu       sync.Mutex
	s        Summary
	events   []Event
	subs     map[chan Event]struct{}
	finished bool
	idle     chan struct{}
	idleOnce sync.Once
	tools    map[string]int // CallID -> index into s.Tools

	// SessionID is the opencode session answering this run. Read by the bot;
	// rebound through the registry when a session has to be recreated.
	sessionID string
}

func newRun(s Summary, sessionID string) *Run {
	r := &Run{
		s:         s,
		subs:      map[chan Event]struct{}{},
		idle:      make(chan struct{}),
		tools:     map[string]int{},
		sessionID: sessionID,
	}
	r.s.Status = StatusRunning
	r.s.StartedAt = now()
	r.s.Tools = []Tool{}
	r.emit("start", map[string]any{
		"agent_id": s.AgentID, "conversation_id": s.ConversationID, "requester_id": s.RequesterID,
	})
	return r
}

func (r *Run) ID() string { return r.s.ID }

func (r *Run) SessionID() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.sessionID
}

// Snapshot is a copy of the current value; safe to hand out.
func (r *Run) Snapshot() Summary {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.snapshotLocked()
}

func (r *Run) snapshotLocked() Summary {
	s := r.s
	// A copy that is never nil: an empty run must say `"tools": []`, not null.
	s.Tools = make([]Tool, len(r.s.Tools))
	copy(s.Tools, r.s.Tools)
	return s
}

// Subscribe returns the current snapshot and a channel carrying every event
// after it. On a finished run the channel is already closed. Call cancel when
// done listening.
func (r *Run) Subscribe() (Summary, <-chan Event, func()) {
	r.mu.Lock()
	defer r.mu.Unlock()
	ch := make(chan Event, subBuffer)
	snap := r.snapshotLocked()
	if r.finished {
		close(ch)
		return snap, ch, func() {}
	}
	r.subs[ch] = struct{}{}
	cancel := func() {
		r.mu.Lock()
		defer r.mu.Unlock()
		if _, ok := r.subs[ch]; ok {
			delete(r.subs, ch)
			close(ch)
		}
	}
	return snap, ch, cancel
}

// emit appends one event and fans it out. Caller holds r.mu.
func (r *Run) emit(kind string, data any) {
	var raw json.RawMessage
	if data != nil {
		raw, _ = json.Marshal(data)
	}
	ev := Event{Seq: len(r.events) + 1, Type: kind, At: now(), Data: raw}
	r.events = append(r.events, ev)
	r.s.Seq = ev.Seq
	for ch := range r.subs {
		select {
		case ch <- ev:
		default:
			// Too far behind. Let it go; it will come back for a snapshot.
			delete(r.subs, ch)
			close(ch)
		}
	}
}

// Thinking appends a slice of the model's reasoning.
func (r *Run) Thinking(delta string) {
	if delta == "" {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.finished {
		return
	}
	r.s.Thinking += delta
	r.emit("thinking", map[string]string{"delta": delta})
}

// Text appends a slice of the answer.
func (r *Run) Text(delta string) {
	if delta == "" {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.finished {
		return
	}
	r.s.Text += delta
	r.emit("text", map[string]string{"delta": delta})
}

// Tool records the latest state of one tool call.
func (r *Run) Tool(t Tool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.finished {
		return
	}
	if i, ok := r.tools[t.CallID]; ok {
		// A later state never loses what an earlier one knew.
		prev := r.s.Tools[i]
		if t.Input == "" {
			t.Input = prev.Input
		}
		if t.Title == "" {
			t.Title = prev.Title
		}
		if t.StartedAt == 0 {
			t.StartedAt = prev.StartedAt
		}
		r.s.Tools[i] = t
	} else {
		r.tools[t.CallID] = len(r.s.Tools)
		r.s.Tools = append(r.s.Tools, t)
	}
	r.emit("tool", t)
}

// Step folds one model step's accounting in.
func (r *Run) Step(u Usage) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.finished {
		return
	}
	r.s.Usage.Input += u.Input
	r.s.Usage.Output += u.Output
	r.s.Usage.Reasoning += u.Reasoning
	r.s.Usage.Cost += u.Cost
	r.s.Usage.Steps++
	r.emit("step", r.s.Usage)
}

// Fail records that the answer went wrong. The run is still finished
// afterwards, once the bot has told the chat.
func (r *Run) Fail(message string) {
	r.mu.Lock()
	if !r.finished && r.s.Status == StatusRunning {
		r.s.Status = StatusError
		r.s.Error = strings.TrimSpace(message)
		r.emit("error", map[string]string{"message": r.s.Error})
	}
	r.mu.Unlock()
	r.MarkIdle()
}

// Cancel records that somebody stopped the run.
func (r *Run) Cancel() {
	r.mu.Lock()
	if !r.finished && r.s.Status == StatusRunning {
		r.s.Status = StatusCancelled
		r.emit("cancelled", nil)
	}
	r.mu.Unlock()
	r.MarkIdle()
}

// MarkIdle signals that the model has stopped producing. Idempotent.
func (r *Run) MarkIdle() { r.idleOnce.Do(func() { close(r.idle) }) }

// Idle is closed once the model has stopped, for whatever reason.
func (r *Run) Idle() <-chan struct{} { return r.idle }

func (r *Run) SetPlaceholder(msgID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.s.PlaceholderMsgID = msgID
}

// finish closes the log. finalMsgID is the chat message holding the answer.
func (r *Run) finish(finalMsgID string) Summary {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.finished {
		return r.snapshotLocked()
	}
	r.finished = true
	if r.s.Status == StatusRunning {
		r.s.Status = StatusDone
	}
	r.s.EndedAt = now()
	r.s.FinalMsgID = finalMsgID
	r.emit("done", map[string]any{"status": r.s.Status, "final_msg_id": finalMsgID, "ended_at": r.s.EndedAt})
	for ch := range r.subs {
		delete(r.subs, ch)
		close(ch)
	}
	r.idleOnce.Do(func() { close(r.idle) })
	return r.snapshotLocked()
}

// ---------------------------------------------------------------- registry ---

// Registry holds the runs this process knows about.
type Registry struct {
	mu        sync.Mutex
	byID      map[string]*Run
	bySession map[string]*Run
	keep      time.Duration
	// OnFinish is called with the final snapshot, for persistence.
	OnFinish func(Summary)
}

// NewRegistry keeps finished runs in memory for `keep` so a viewer that
// reconnects just after the end still gets the log rather than a 404.
func NewRegistry(keep time.Duration) *Registry {
	if keep <= 0 {
		keep = 30 * time.Minute
	}
	return &Registry{byID: map[string]*Run{}, bySession: map[string]*Run{}, keep: keep}
}

// Start opens a run and binds it to the opencode session answering it, so
// events from that session can be routed here.
func (g *Registry) Start(s Summary, sessionID string) *Run {
	s.ID = newID()
	r := newRun(s, sessionID)
	g.mu.Lock()
	defer g.mu.Unlock()
	g.byID[s.ID] = r
	if sessionID != "" {
		g.bySession[sessionID] = r
	}
	return r
}

// Rebind moves a run to a different session (the old one was forgotten).
func (g *Registry) Rebind(r *Run, sessionID string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	r.mu.Lock()
	old := r.sessionID
	r.sessionID = sessionID
	r.mu.Unlock()
	if old != "" && g.bySession[old] == r {
		delete(g.bySession, old)
	}
	if sessionID != "" {
		g.bySession[sessionID] = r
	}
}

func (g *Registry) Get(id string) (*Run, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	r, ok := g.byID[id]
	return r, ok
}

func (g *Registry) BySession(sessionID string) (*Run, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	r, ok := g.bySession[sessionID]
	return r, ok
}

// Active lists runs still producing.
func (g *Registry) Active() []*Run {
	g.mu.Lock()
	defer g.mu.Unlock()
	out := make([]*Run, 0, len(g.bySession))
	for _, r := range g.bySession {
		out = append(out, r)
	}
	return out
}

// Finish closes the run, unbinds its session, persists the snapshot and
// schedules eviction from memory.
func (g *Registry) Finish(r *Run, finalMsgID string) Summary {
	sum := r.finish(finalMsgID)
	g.mu.Lock()
	if sid := r.sessionID; sid != "" && g.bySession[sid] == r {
		delete(g.bySession, sid)
	}
	g.mu.Unlock()
	if g.OnFinish != nil {
		g.OnFinish(sum)
	}
	time.AfterFunc(g.keep, func() {
		g.mu.Lock()
		defer g.mu.Unlock()
		if g.byID[sum.ID] == r {
			delete(g.byID, sum.ID)
		}
	})
	return sum
}

func newID() string {
	raw := make([]byte, 10)
	_, _ = rand.Read(raw)
	return "run_" + strings.ToLower(base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(raw))
}

func now() int64 { return time.Now().UnixMilli() }
