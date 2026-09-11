package bot

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/his1devil/yptd/server/internal/run"
)

func ev(kind string, props string) Event {
	return Event{Type: kind, Properties: json.RawMessage(props)}
}

func fresh(prompt string) (*run.Run, *stream) {
	g := run.NewRegistry(time.Minute)
	r := g.Start(run.Summary{AgentID: "a", ConversationID: "sg_1", Prompt: prompt}, "ses_1")
	return r, newStream(prompt)
}

// The shapes below are copied from a real opencode 1.18 event stream.

func TestDeltasAndSnapshotsDoNotDoubleTheText(t *testing.T) {
	r, st := fresh("问题")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_a","role":"assistant"}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_t","messageID":"msg_a","type":"text","text":""}}`))
	st.apply(r, ev("message.part.delta", `{"sessionID":"ses_1","messageID":"msg_a","partID":"prt_t","field":"text","delta":"你"}`))
	st.apply(r, ev("message.part.delta", `{"sessionID":"ses_1","messageID":"msg_a","partID":"prt_t","field":"text","delta":"好"}`))
	// The snapshot that repeats everything so far must add nothing...
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_t","messageID":"msg_a","type":"text","text":"你好"}}`))
	// ...and one that is ahead of the deltas contributes only its tail.
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_t","messageID":"msg_a","type":"text","text":"你好啊"}}`))
	if got := r.Snapshot().Text; got != "你好啊" {
		t.Fatalf("text %q", got)
	}
}

func TestReasoningGoesToThinkingNotToTheAnswer(t *testing.T) {
	r, st := fresh("问题")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_a","role":"assistant"}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_r","messageID":"msg_a","type":"reasoning","text":"","time":{"start":1}}}`))
	st.apply(r, ev("message.part.delta", `{"sessionID":"ses_1","messageID":"msg_a","partID":"prt_r","field":"text","delta":"The user asks"}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_t","messageID":"msg_a","type":"text","text":""}}`))
	st.apply(r, ev("message.part.delta", `{"sessionID":"ses_1","messageID":"msg_a","partID":"prt_t","field":"text","delta":"答案"}`))
	s := r.Snapshot()
	if s.Thinking != "The user asks" || s.Text != "答案" {
		t.Fatalf("thinking %q text %q", s.Thinking, s.Text)
	}
}

func TestTheEchoedPromptIsNotTheAnswer(t *testing.T) {
	// opencode reports the user's message as a text part too, and in the
	// real stream it comes first.
	r, st := fresh("珠峰多高")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_u","role":"user"}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_u","messageID":"msg_u","type":"text","text":"珠峰多高"}}`))
	// And even when the role has not been announced yet.
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_u2","messageID":"msg_u2","type":"text","text":"珠峰多高"}}`))
	if got := r.Snapshot().Text; got != "" {
		t.Fatalf("the question leaked into the answer: %q", got)
	}
}

func TestDeltasBeforeTheirPartAreHeldUntilTheKindIsKnown(t *testing.T) {
	r, st := fresh("q")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_a","role":"assistant"}}`))
	st.apply(r, ev("message.part.delta", `{"sessionID":"ses_1","messageID":"msg_a","partID":"prt_r","field":"text","delta":"hmm"}`))
	if r.Snapshot().Thinking != "" || r.Snapshot().Text != "" {
		t.Fatal("an unclassified delta must not be shown anywhere yet")
	}
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_r","messageID":"msg_a","type":"reasoning","text":"hmm","time":{"start":1}}}`))
	s := r.Snapshot()
	if s.Thinking != "hmm" || s.Text != "" {
		t.Fatalf("thinking %q text %q", s.Thinking, s.Text)
	}
}

func TestToolStatesUpsertAndKeepTheInput(t *testing.T) {
	r, st := fresh("q")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_a","role":"assistant"}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_c","messageID":"msg_a","type":"tool","tool":"bash","callID":"call_1","state":{"status":"pending","input":{},"raw":""}}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_c","messageID":"msg_a","type":"tool","tool":"bash","callID":"call_1","state":{"status":"running","input":{"command":"date"},"title":"date","time":{"start":10}}}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"prt_c","messageID":"msg_a","type":"tool","tool":"bash","callID":"call_1","state":{"status":"completed","input":{"command":"date"},"output":"Thu","title":"date","metadata":{},"time":{"start":10,"end":20}}}}`))
	s := r.Snapshot()
	if len(s.Tools) != 1 {
		t.Fatalf("one call, three states, one row: %+v", s.Tools)
	}
	tool := s.Tools[0]
	if tool.Status != "completed" || tool.Output != "Thu" || !strings.Contains(tool.Input, "date") || tool.StartedAt != 10 || tool.EndedAt != 20 {
		t.Fatalf("%+v", tool)
	}
}

func TestStepFinishAccumulatesUsageAndIdleEndsTheRun(t *testing.T) {
	r, st := fresh("q")
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"p1","messageID":"msg_a","type":"step-finish","reason":"tool-calls","tokens":{"input":100,"output":10,"reasoning":5,"cache":{"read":0,"write":0}},"cost":0.01}}`))
	st.apply(r, ev("message.part.updated", `{"sessionID":"ses_1","part":{"id":"p2","messageID":"msg_a","type":"step-finish","reason":"stop","tokens":{"input":50,"output":20,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.02}}`))
	u := r.Snapshot().Usage
	if u.Input != 150 || u.Output != 30 || u.Steps != 2 || u.Cost < 0.029 {
		t.Fatalf("%+v", u)
	}
	select {
	case <-r.Idle():
		t.Fatal("not idle yet")
	default:
	}
	st.apply(r, ev("session.idle", `{"sessionID":"ses_1"}`))
	select {
	case <-r.Idle():
	default:
		t.Fatal("session.idle must release the wait")
	}
}

func TestAnAssistantErrorFailsTheRun(t *testing.T) {
	r, st := fresh("q")
	st.apply(r, ev("message.updated", `{"sessionID":"ses_1","info":{"id":"msg_a","role":"assistant","error":{"name":"APIError","data":{"message":"quota exceeded"}}}}`))
	s := r.Snapshot()
	if s.Status != run.StatusError || s.Error != "quota exceeded" {
		t.Fatalf("%+v", s)
	}
}

func TestClipCutsOnARuneBoundary(t *testing.T) {
	got := clip(strings.Repeat("字", 100), 7)
	if !strings.HasPrefix(got, "字字") || strings.Contains(got, "�") {
		t.Fatalf("%q", got)
	}
}
