package bot

import (
	"encoding/json"
	"strings"

	"github.com/his1devil/yptd/server/internal/run"
)

// stream folds opencode's events for one run into the run's log.
//
// opencode reports a reply two ways at once: `message.part.delta` carries each
// new slice of a text or reasoning part, and `message.part.updated` carries
// the whole part again whenever anything about it changes. Reading both
// naively doubles the text. The rule here is to count what has been emitted
// per part and only ever emit what lies beyond that -- so a delta advances the
// count, and a snapshot longer than the count contributes just its tail. That
// also survives a stream that hands us snapshots only, or deltas only.
type stream struct {
	// prompt is what we sent; opencode echoes it back as a text part of the
	// user message, and that must not be mistaken for the answer.
	prompt  string
	roles   map[string]string   // messageID -> role
	kinds   map[string]string   // partID -> text | reasoning | other
	emitted map[string]int      // partID -> bytes already emitted
	pending map[string][]string // deltas that arrived before their part's kind
}

func newStream(prompt string) *stream {
	return &stream{
		prompt:  prompt,
		roles:   map[string]string{},
		kinds:   map[string]string{},
		emitted: map[string]int{},
		pending: map[string][]string{},
	}
}

// Output limits per tool call. The chat is not the place for a whole log
// file; the client shows a head and says how much was cut.
const (
	maxToolInput  = 2000
	maxToolOutput = 4000
)

type partSnapshot struct {
	ID        string `json:"id"`
	MessageID string `json:"messageID"`
	Type      string `json:"type"`
	Text      string `json:"text"`
	Synthetic bool   `json:"synthetic"`
	Ignored   bool   `json:"ignored"`
	Tool      string `json:"tool"`
	CallID    string `json:"callID"`
	State     struct {
		Status string          `json:"status"`
		Input  json.RawMessage `json:"input"`
		Output string          `json:"output"`
		Title  string          `json:"title"`
		Error  string          `json:"error"`
		Time   struct {
			Start int64 `json:"start"`
			End   int64 `json:"end"`
		} `json:"time"`
	} `json:"state"`
	Tokens struct {
		Input     int64 `json:"input"`
		Output    int64 `json:"output"`
		Reasoning int64 `json:"reasoning"`
	} `json:"tokens"`
	Cost float64 `json:"cost"`
}

func (st *stream) apply(r *run.Run, ev Event) {
	switch ev.Type {
	case "message.updated":
		var p struct {
			Info struct {
				ID    string          `json:"id"`
				Role  string          `json:"role"`
				Error json.RawMessage `json:"error"`
			} `json:"info"`
		}
		if json.Unmarshal(ev.Properties, &p) != nil {
			return
		}
		st.roles[p.Info.ID] = p.Info.Role
		if p.Info.Role == "assistant" && len(p.Info.Error) > 2 {
			r.Fail(errorText(p.Info.Error))
		}
	case "message.part.updated":
		var p struct {
			Part partSnapshot `json:"part"`
		}
		if json.Unmarshal(ev.Properties, &p) != nil {
			return
		}
		st.part(r, p.Part)
	case "message.part.delta":
		var p struct {
			MessageID string `json:"messageID"`
			PartID    string `json:"partID"`
			Field     string `json:"field"`
			Delta     string `json:"delta"`
		}
		if json.Unmarshal(ev.Properties, &p) != nil {
			return
		}
		st.delta(r, p.MessageID, p.PartID, p.Field, p.Delta)
	case "session.idle":
		r.MarkIdle()
	case "session.error":
		var p struct {
			Error json.RawMessage `json:"error"`
		}
		_ = json.Unmarshal(ev.Properties, &p)
		r.Fail(errorText(p.Error))
	}
}

func (st *stream) part(r *run.Run, p partSnapshot) {
	if st.roles[p.MessageID] == "user" {
		return
	}
	switch p.Type {
	case "text", "reasoning":
		if p.Type == "text" && p.Text == st.prompt {
			// The echo of the question, before its message's role was known.
			st.roles[p.MessageID] = "user"
			return
		}
		kind := p.Type
		if p.Synthetic || p.Ignored {
			kind = "other"
		}
		st.kinds[p.ID] = kind
		for _, d := range st.pending[p.ID] {
			st.emit(r, kind, d)
			st.emitted[p.ID] += len(d)
		}
		delete(st.pending, p.ID)
		if done := st.emitted[p.ID]; len(p.Text) > done {
			st.emit(r, kind, p.Text[done:])
			st.emitted[p.ID] = len(p.Text)
		}
	case "tool":
		t := run.Tool{
			CallID: p.CallID, Name: p.Tool, Status: p.State.Status, Title: p.State.Title,
			Output: clip(p.State.Output, maxToolOutput), Error: clip(p.State.Error, maxToolOutput),
			StartedAt: p.State.Time.Start, EndedAt: p.State.Time.End,
		}
		if len(p.State.Input) > 2 {
			t.Input = clip(string(p.State.Input), maxToolInput)
		}
		r.Tool(t)
	case "step-finish":
		r.Step(run.Usage{Input: p.Tokens.Input, Output: p.Tokens.Output, Reasoning: p.Tokens.Reasoning, Cost: p.Cost})
	}
}

func (st *stream) delta(r *run.Run, messageID, partID, field, delta string) {
	if field != "text" || delta == "" || st.roles[messageID] == "user" {
		return
	}
	kind, known := st.kinds[partID]
	if !known {
		st.pending[partID] = append(st.pending[partID], delta)
		return
	}
	st.emit(r, kind, delta)
	st.emitted[partID] += len(delta)
}

func (st *stream) emit(r *run.Run, kind, text string) {
	switch kind {
	case "text":
		r.Text(text)
	case "reasoning":
		r.Thinking(text)
	}
}

// errorText pulls a sentence out of opencode's error object, whatever its
// exact shape.
func errorText(raw json.RawMessage) string {
	var e struct {
		Name string `json:"name"`
		Data struct {
			Message string `json:"message"`
		} `json:"data"`
		Message string `json:"message"`
	}
	_ = json.Unmarshal(raw, &e)
	msg := e.Data.Message
	if msg == "" {
		msg = e.Message
	}
	if msg == "" {
		msg = e.Name
	}
	if msg == "" {
		msg = strings.TrimSpace(string(raw))
	}
	return oneLine(msg)
}

func clip(s string, max int) string {
	if len(s) <= max {
		return s
	}
	// Cut on a rune boundary so the tail is not half a character.
	cut := max
	for cut > 0 && cut < len(s) && (s[cut]&0xC0) == 0x80 {
		cut--
	}
	return s[:cut] + "…（截断）"
}
