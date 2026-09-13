package bot

import (
	"context"
	"strings"
	"sync"
	"testing"

	"github.com/his1devil/yptd/server/internal/config"
)

func TestWorthStaysQuietUnderThreshold(t *testing.T) {
	if got := Worth(2.9, 0, false, 3); got != Calm {
		t.Fatalf("2.9%% under a 3%% threshold should be Calm, got %v", got)
	}
}

func TestWorthAnnouncesFirstCrossing(t *testing.T) {
	got := Worth(3.4, 0, false, 3)
	if got != Fresh || !got.Say() || !got.Keep() {
		t.Fatalf("first crossing should be Fresh and spoken: %v", got)
	}
}

func TestWorthDoesNotRepeatTheSameMove(t *testing.T) {
	// Drifting from 3.4 to 5.1 is the same event, not a second one.
	got := Worth(5.1, 3.4, true, 3)
	if got != Steady || got.Say() || !got.Keep() {
		t.Fatalf("a drift under one more threshold should be Steady: %v", got)
	}
}

func TestWorthAnnouncesEscalation(t *testing.T) {
	// 3.4 -> 6.5 has grown by a further threshold, so it is worth a line.
	if got := Worth(6.5, 3.4, true, 3); got != Wider {
		t.Fatalf("a move that grew by another whole threshold should be Wider, got %v", got)
	}
}

func TestWorthForgetsWhenTheMoveFades(t *testing.T) {
	// Coming back inside the band clears the memory, so the next run out
	// alerts again instead of being swallowed as "already told you".
	got := Worth(0.4, 5.0, true, 3)
	if got != Calm || got.Keep() {
		t.Fatalf("fading back under the threshold should be Calm and forgotten: %v", got)
	}
	if got := Worth(3.5, 0, false, 3); !got.Say() {
		t.Fatal("after fading, a fresh crossing should be announced")
	}
}

func TestWorthTreatsAReversalAsNews(t *testing.T) {
	// -4 after +4 is an 8-point swing but also a change of direction; either
	// way it is not the thing we already said.
	if got := Worth(-4, 4, true, 3); got != Turned {
		t.Fatalf("crossing from up to down should be Turned, got %v", got)
	}
	// Same sign, same size: nothing new.
	if got := Worth(4.1, 4, true, 3); got.Say() {
		t.Fatalf("an unchanged move should not be repeated, got %v", got)
	}
}

// --- what the room actually reads ------------------------------------------

func TestAnnounceSaysOneSymbolInOneLine(t *testing.T) {
	text := Announce([]Moved{{Quote: Quote{Symbol: "NVDA.US", ChangePct: -7.85, Last: 218.29}, Move: Fresh}})
	want := "**NVDA.US** 盘中跌 `-7.85%` · 现价 218.29"
	if text != want {
		t.Fatalf("one symbol should need no heading:\n got %q\nwant %q", text, want)
	}
}

func TestAnnounceLeadsWithTheBiggestMove(t *testing.T) {
	text := Announce([]Moved{
		{Quote: Quote{Symbol: "700.HK", ChangePct: 3.2, Last: 428.4}, Move: Fresh},
		{Quote: Quote{Symbol: "NVDA.US", ChangePct: -7.85, Last: 218.29}, Move: Fresh},
	})
	nvda, tencent := strings.Index(text, "NVDA.US"), strings.Index(text, "700.HK")
	if nvda < 0 || tencent < 0 || nvda > tencent {
		t.Fatalf("the larger move should come first:\n%s", text)
	}
	if !strings.HasPrefix(text, "**盘中异动**\n") {
		t.Fatalf("several symbols should get a heading:\n%s", text)
	}
	if !strings.Contains(text, "`-7.85%`") || !strings.Contains(text, "`+3.20%`") {
		t.Fatalf("percentages need their sign and backticks so the client can colour them:\n%s", text)
	}
	if strings.Contains(text, "↑") || strings.Contains(text, "↓") {
		t.Fatalf("the sign already carries the direction; no arrows:\n%s", text)
	}
	if !strings.Contains(text, "428.40") || strings.Contains(text, "428.400") {
		t.Fatalf("prices keep two decimals, not three:\n%s", text)
	}
}

func TestAnnounceTakesItsVerbFromTheMove(t *testing.T) {
	for _, c := range []struct {
		move Move
		pct  float64
		want string
	}{
		{Fresh, -7.85, "盘中跌"},
		{Fresh, 7.85, "盘中涨"},
		{Wider, -11.2, "盘中跌幅扩大至"},
		{Wider, 11.2, "盘中涨幅扩大至"},
		{Turned, 3.2, "盘中由跌转涨"},
		{Turned, -3.2, "盘中由涨转跌"},
	} {
		text := Announce([]Moved{{Quote: Quote{Symbol: "X.US", ChangePct: c.pct, Last: 1}, Move: c.move}})
		if !strings.Contains(text, c.want) {
			t.Fatalf("%v at %+.2f should read %q:\n%s", c.move, c.pct, c.want, text)
		}
	}
}

func TestTrimNumReadsLikeMoney(t *testing.T) {
	for in, want := range map[float64]string{
		428.4: "428.40", 218.29: "218.29", 100: "100.00", 0.125: "0.125", 1234.5: "1234.50",
	} {
		if got := trimNum(in); got != want {
			t.Fatalf("%v should print as %q, got %q", in, want, got)
		}
	}
}

func TestAnnounceSaysNothingAboutNothing(t *testing.T) {
	if got := Announce(nil); got != "" {
		t.Fatalf("no moves should produce no message, got %q", got)
	}
}

// --- the loop around the decision -------------------------------------------

type fakeQuotes struct {
	rounds [][]Quote
	err    error
	n      int
}

func (f *fakeQuotes) Quotes(context.Context, []string) ([]Quote, error) {
	if f.err != nil {
		return nil, f.err
	}
	if f.n >= len(f.rounds) {
		return nil, nil
	}
	r := f.rounds[f.n]
	f.n++
	return r, nil
}

type fakeGroups struct {
	ids []string
	err error
}

func (f fakeGroups) JoinedGroups(context.Context, string) ([]string, error) { return f.ids, f.err }

type fakeSender struct {
	mu   sync.Mutex
	sent []string
}

func (f *fakeSender) SendText(_ context.Context, _, _, _, groupID, text, _ string) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sent = append(f.sent, groupID+"|"+text)
	return "m1", nil
}

func (f *fakeSender) SendCustom(context.Context, string, string, string, string, string, string) (string, error) {
	return "", nil
}

func watcherFor(q Quotes, g Groups, s Sender) *Watcher {
	return NewWatcher(config.Agent{
		UserID: "agentcharlie", Nickname: "Charlie",
		Watch: config.Watch{Symbols: []string{"700.HK"}, Threshold: 3, Every: "1m"},
	}, q, s, g, nil)
}

func TestPollPostsToEveryGroupOnce(t *testing.T) {
	q := &fakeQuotes{rounds: [][]Quote{{{Symbol: "700.HK", ChangePct: 4.2, Last: 400}}}}
	s := &fakeSender{}
	w := watcherFor(q, fakeGroups{ids: []string{"g1", "g2"}}, s)
	w.poll(context.Background())
	if len(s.sent) != 2 {
		t.Fatalf("one move, two groups, want 2 messages, got %d", len(s.sent))
	}
	// Second poll with the same number says nothing.
	q.rounds = append(q.rounds, []Quote{{Symbol: "700.HK", ChangePct: 4.3, Last: 400}})
	w.poll(context.Background())
	if len(s.sent) != 2 {
		t.Fatalf("an unchanged move should not repeat, got %d messages", len(s.sent))
	}
}

func TestPollSkipsHaltedSymbols(t *testing.T) {
	s := &fakeSender{}
	w := watcherFor(&fakeQuotes{rounds: [][]Quote{{{Symbol: "700.HK", ChangePct: 9, Last: 400, Status: "Suspend"}}}},
		fakeGroups{ids: []string{"g1"}}, s)
	w.poll(context.Background())
	if len(s.sent) != 0 {
		t.Fatalf("a halted symbol cannot move; want silence, got %v", s.sent)
	}
}

func TestPollRetriesWhenTheGroupLookupFails(t *testing.T) {
	// A move consumed by a failed send must not be swallowed: the same move
	// on the next poll should still go out.
	q := &fakeQuotes{rounds: [][]Quote{
		{{Symbol: "700.HK", ChangePct: 4.2, Last: 400}},
		{{Symbol: "700.HK", ChangePct: 4.2, Last: 400}},
	}}
	s := &fakeSender{}
	w := watcherFor(q, fakeGroups{err: context.DeadlineExceeded}, s)
	w.poll(context.Background())
	w.groups = fakeGroups{ids: []string{"g1"}}
	w.poll(context.Background())
	if len(s.sent) != 1 {
		t.Fatalf("the alert should survive a failed group lookup, got %d messages", len(s.sent))
	}
}

func TestWatchOffIsOff(t *testing.T) {
	s := &fakeSender{}
	w := NewWatcher(config.Agent{UserID: "agentbot"}, &fakeQuotes{}, s, fakeGroups{ids: []string{"g1"}}, nil)
	w.Run(context.Background())
	if len(s.sent) != 0 {
		t.Fatalf("an agent with no symbols should never post, got %v", s.sent)
	}
}

func TestTunedFloorsTheInterval(t *testing.T) {
	if got := (config.Watch{Symbols: []string{"x"}, Every: "1s"}).Interval(); got.Seconds() != 30 {
		t.Fatalf("a one-second poll should be floored to 30s, got %v", got)
	}
	if got := (config.Watch{Symbols: []string{"x"}}).Interval(); got.Minutes() != 2 {
		t.Fatalf("an unset interval should default to 2m, got %v", got)
	}
	if got := (config.Watch{Symbols: []string{"x"}}).Tuned().Threshold; got != 3 {
		t.Fatalf("an unset threshold should default to 3, got %v", got)
	}
}

func TestParseQuotesDropsUnusableRows(t *testing.T) {
	got, err := parseQuotes([]byte(`[
      {"symbol":"700.HK","last":"428.400","change_percentage":"0.66","status":"Normal"},
      {"symbol":"BAD.US","last":"","change_percentage":"","status":"Normal"},
      {"symbol":"","last":"1","change_percentage":"1","status":"Normal"}
    ]`))
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Symbol != "700.HK" || got[0].Last != 428.4 || got[0].ChangePct != 0.66 {
		t.Fatalf("only the usable row should survive: %+v", got)
	}
}

func TestWatchCanBeLimitedToOneRoom(t *testing.T) {
	s := &fakeSender{}
	w := NewWatcher(config.Agent{
		UserID: "agentcharlie", Nickname: "Charlie",
		Watch: config.Watch{Symbols: []string{"700.HK"}, Threshold: 3, Groups: []string{"g2"}},
	}, &fakeQuotes{rounds: [][]Quote{{{Symbol: "700.HK", ChangePct: 5, Last: 400}}}},
		s, fakeGroups{ids: []string{"g1", "g2", "g3"}}, nil)
	w.poll(context.Background())
	if len(s.sent) != 1 || !strings.HasPrefix(s.sent[0], "g2|") {
		t.Fatalf("only the allowed room should hear it, got %v", s.sent)
	}
}

func TestEmptyGroupListMeansEverywhere(t *testing.T) {
	s := &fakeSender{}
	w := watcherFor(&fakeQuotes{rounds: [][]Quote{{{Symbol: "700.HK", ChangePct: 5, Last: 400}}}},
		fakeGroups{ids: []string{"g1", "g2"}}, s)
	w.poll(context.Background())
	if len(s.sent) != 2 {
		t.Fatalf("no allowlist means every joined group, got %d", len(s.sent))
	}
}
