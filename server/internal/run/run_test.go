package run

import (
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func TestSnapshotThenTailConvergesWithWatchingFromTheStart(t *testing.T) {
	// 晚到的观众拿到快照 + 之后的事件，得和从头看的人看到一样的东西。
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a", ConversationID: "sg_1", Prompt: "hi"}, "ses_1")
	_, early, cancelEarly := r.Subscribe()
	defer cancelEarly()

	r.Thinking("想")
	r.Text("你")
	r.Tool(Tool{CallID: "c1", Name: "bash", Status: "running", Input: `{"cmd":"date"}`})

	snap, late, cancelLate := r.Subscribe()
	defer cancelLate()
	if snap.Text != "你" || snap.Thinking != "想" || len(snap.Tools) != 1 || snap.Seq != 4 {
		t.Fatalf("snapshot %+v", snap)
	}

	r.Text("好")
	r.Tool(Tool{CallID: "c1", Name: "bash", Status: "completed", Output: "now"})
	g.Finish(r, "msg9")

	var earlyText, lateText string
	for ev := range early {
		if ev.Type == "text" {
			earlyText += string(ev.Data)
		}
	}
	for ev := range late {
		if ev.Type == "text" {
			lateText += string(ev.Data)
		}
	}
	if lateText != `{"delta":"好"}` {
		t.Fatalf("late viewer should only get the tail, got %q", lateText)
	}
	if earlyText != `{"delta":"你"}{"delta":"好"}` {
		t.Fatalf("early viewer text %q", earlyText)
	}
	final := r.Snapshot()
	if final.Status != StatusDone || final.FinalMsgID != "msg9" || final.Text != "你好" {
		t.Fatalf("final %+v", final)
	}
	if final.Tools[0].Status != "completed" || final.Tools[0].Input != `{"cmd":"date"}` {
		t.Fatalf("a later tool state must keep the earlier input: %+v", final.Tools[0])
	}
}

func TestSubscribeAfterFinishGetsSnapshotAndAClosedChannel(t *testing.T) {
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a"}, "ses_2")
	r.Text("done")
	g.Finish(r, "m1")
	snap, ch, cancel := r.Subscribe()
	defer cancel()
	if snap.Text != "done" {
		t.Fatalf("snapshot %+v", snap)
	}
	if _, open := <-ch; open {
		t.Fatal("channel of a finished run must be closed")
	}
}

func TestFailAndCancelBothStopTheWait(t *testing.T) {
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a"}, "ses_3")
	r.Fail("boom")
	select {
	case <-r.Idle():
	default:
		t.Fatal("Fail must release whoever waits for idle")
	}
	sum := g.Finish(r, "m")
	if sum.Status != StatusError || sum.Error != "boom" {
		t.Fatalf("%+v", sum)
	}

	r2 := g.Start(Summary{AgentID: "a"}, "ses_4")
	r2.Cancel()
	if g.Finish(r2, "").Status != StatusCancelled {
		t.Fatal("cancel must stick")
	}
}

func TestRegistryRoutesBySessionAndUnbindsOnFinish(t *testing.T) {
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a"}, "ses_5")
	if got, ok := g.BySession("ses_5"); !ok || got != r {
		t.Fatal("session should route to the run")
	}
	g.Rebind(r, "ses_6")
	if _, ok := g.BySession("ses_5"); ok {
		t.Fatal("old session must be unbound")
	}
	if got, ok := g.BySession("ses_6"); !ok || got != r {
		t.Fatal("new session should route")
	}
	g.Finish(r, "m")
	if _, ok := g.BySession("ses_6"); ok {
		t.Fatal("finished runs must not receive events")
	}
	if _, ok := g.Get(r.ID()); !ok {
		t.Fatal("finished runs stay readable for a while")
	}
}

func TestSlowSubscriberIsDroppedNotBlocking(t *testing.T) {
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a"}, "ses_7")
	_, ch, cancel := r.Subscribe()
	defer cancel()
	for i := 0; i < subBuffer+10; i++ {
		r.Text("x") // must never block even though nobody reads
	}
	n := 0
	for range ch {
		n++
	}
	if n != subBuffer {
		t.Fatalf("expected the buffer's worth then a close, got %d", n)
	}
}

func TestFreshSnapshotHasAnEmptyToolListNotNull(t *testing.T) {
	g := NewRegistry(time.Minute)
	r := g.Start(Summary{AgentID: "a"}, "ses_8")
	raw, _ := json.Marshal(r.Snapshot())
	if !strings.Contains(string(raw), `"tools":[]`) {
		t.Fatalf("tools must marshal as an empty list: %s", raw)
	}
}
