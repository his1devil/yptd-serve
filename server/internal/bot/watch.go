package bot

import (
	"context"
	"fmt"
	"log/slog"
	"math"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/his1devil/yptd/server/internal/config"
)

// Watcher polls one agent's symbols and speaks up in its groups when one of
// them moves far enough to be worth interrupting a room over.
//
// It deliberately does not go through opencode. A price alert is a fixed
// sentence built from three numbers: routing it through the model would cost
// a call per push, make the wording drift, and — worse — pour unprompted
// turns into the group's shared agent session, so the next person who asks
// Charlie something finds its context full of broadcasts nobody sent.
// Interpretation is what the agent is for, and people ask for it by name.
type Watcher struct {
	agent  config.Agent
	watch  config.Watch
	quotes Quotes
	send   Sender
	groups Groups
	log    *slog.Logger

	mu sync.Mutex
	// seen is the last percentage this watcher actually announced per
	// symbol. Absent means "nothing outstanding".
	seen map[string]float64
}

// Groups answers where an agent can currently speak. Read every poll rather
// than cached: an agent joins and leaves rooms without this service hearing.
type Groups interface {
	JoinedGroups(ctx context.Context, userID string) ([]string, error)
}

func NewWatcher(agent config.Agent, quotes Quotes, send Sender, groups Groups, log *slog.Logger) *Watcher {
	if log == nil {
		log = slog.Default()
	}
	return &Watcher{
		agent:  agent,
		watch:  agent.Watch.Tuned(),
		quotes: quotes,
		send:   send,
		groups: groups,
		log:    log,
		seen:   map[string]float64{},
	}
}

// Run polls until ctx is done. Returns immediately when the watch is off.
func (w *Watcher) Run(ctx context.Context) {
	if !w.watch.On() {
		return
	}
	every := w.watch.Interval()
	w.log.Info("watch: started", "agent", w.agent.UserID,
		"symbols", strings.Join(w.watch.Symbols, " "), "threshold", w.watch.Threshold, "every", every)
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		// First pass runs immediately so a restart does not sit quiet for a
		// whole interval; nothing is announced unless a symbol is already
		// past the threshold, which is the same condition as any other pass.
		w.poll(ctx)
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
	}
}

func (w *Watcher) poll(ctx context.Context) {
	quotes, err := w.quotes.Quotes(ctx, w.watch.Symbols)
	if err != nil {
		// Every poll failing the same way is the signature of an expired CLI
		// login. Log it rather than pushing anything: silence is the correct
		// output when the numbers are unknown.
		w.log.Warn("watch: quotes", "err", err, "agent", w.agent.UserID)
		return
	}
	due := w.decide(quotes)
	if len(due) == 0 {
		return
	}
	groups, err := w.groups.JoinedGroups(ctx, w.agent.UserID)
	if err != nil {
		w.log.Warn("watch: joined groups", "err", err, "agent", w.agent.UserID)
		// The decision already consumed these moves. Put them back so the
		// next poll retries instead of swallowing the alert.
		w.forget(due)
		return
	}
	groups = w.watch.Rooms(groups)
	if len(groups) == 0 {
		w.forget(due)
		return
	}
	text := Announce(due)
	for _, g := range groups {
		if _, err := w.send.SendText(ctx, w.agent.UserID, w.agent.Nickname, "", g, text, ""); err != nil {
			w.log.Error("watch: send", "err", err, "agent", w.agent.UserID, "group", g)
		}
	}
	w.log.Info("watch: announced", "agent", w.agent.UserID, "symbols", len(due), "groups", len(groups))
}

// decide picks the quotes worth announcing and records them as announced.
func (w *Watcher) decide(quotes []Quote) []Moved {
	w.mu.Lock()
	defer w.mu.Unlock()
	var due []Moved
	for _, q := range quotes {
		if q.Halted() {
			continue
		}
		prev, had := w.seen[q.Symbol]
		m := Worth(q.ChangePct, prev, had, w.watch.Threshold)
		if m.Keep() {
			w.seen[q.Symbol] = q.ChangePct
		} else {
			delete(w.seen, q.Symbol)
		}
		if m.Say() {
			due = append(due, Moved{Quote: q, Move: m})
		}
	}
	return due
}

// forget undoes decide's bookkeeping for moves that never made it out.
func (w *Watcher) forget(moves []Moved) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, m := range moves {
		delete(w.seen, m.Symbol)
	}
}

// Move is what happened to a symbol since the last thing this watcher said
// about it. It decides both whether to speak and which verb to use.
type Move int

const (
	// Calm: 没过线。不说，也不记——回落之后再冲出去要当新事件报。
	Calm Move = iota
	// Steady: 过了线，但还是上回说的那一档。不说，继续记着。
	Steady
	// Fresh: 头一回越过阈值。
	Fresh
	// Wider: 同向又走了整整一个阈值。
	Wider
	// Turned: 穿过零翻了向。
	Turned
)

// Say reports whether this move is worth interrupting a room over.
func (m Move) Say() bool { return m >= Fresh }

// Keep reports whether the symbol stays on the outstanding list.
func (m Move) Keep() bool { return m >= Steady }

// Worth classifies a move, given the last percentage announced for this symbol.
//
//   - Under the threshold it is Calm: nothing is said and the symbol is
//     forgotten, so a move that comes back and goes again alerts again.
//   - Over the threshold with nothing outstanding it is Fresh.
//   - Over the threshold with something outstanding it only speaks again once
//     the move has grown by another whole threshold (Wider) or flipped sign
//     (Turned). A stock running from 3% to 9% is worth three sentences, not
//     one every two minutes.
//
// This is also why the watch needs no trading calendar. After the close the
// percentage stops changing, so it stops clearing the threshold gap and the
// room goes quiet on its own — no holiday table to keep current, and no
// market that silently drops out because its table was wrong.
func Worth(pct, prev float64, had bool, threshold float64) Move {
	if threshold <= 0 {
		threshold = 3
	}
	if math.Abs(pct) < threshold {
		return Calm
	}
	if !had {
		return Fresh
	}
	// A reversal through zero is a new event even when it stays large.
	if (pct > 0) != (prev > 0) {
		return Turned
	}
	if math.Abs(pct-prev) >= threshold {
		return Wider
	}
	return Steady
}

// Moved is a quote together with why it is being announced.
type Moved struct {
	Quote
	Move Move
}

// verb writes what happened in words. The percentage carries the sign and the
// client colours it, so the verb never repeats the direction as an arrow.
func (m Moved) verb() string {
	up := m.ChangePct > 0
	switch m.Move {
	case Wider:
		if up {
			return "涨幅扩大至"
		}
		return "跌幅扩大至"
	case Turned:
		if up {
			return "由跌转涨"
		}
		return "由涨转跌"
	default:
		if up {
			return "涨"
		}
		return "跌"
	}
}

// Announce writes the message a watch posts.
//
// One symbol gets one line with no heading: that is the common case, and a
// heading over a single bullet is ceremony. Several get a heading and a list,
// sorted with the biggest move first so the worst news is the line you read.
//
// The markup is what the client's Rich renderer actually parses: **bold** and
// `code`, nothing else. Lists and headings are not parsed there, so the bullet
// is a literal 「·」 and the heading is just a bold line. Percentages go in
// backticks with an explicit sign — that is the shape Rich colours 红涨绿跌.
func Announce(moves []Moved) string {
	if len(moves) == 0 {
		return ""
	}
	sorted := append([]Moved(nil), moves...)
	sort.Slice(sorted, func(i, j int) bool {
		return math.Abs(sorted[i].ChangePct) > math.Abs(sorted[j].ChangePct)
	})
	if len(sorted) == 1 {
		m := sorted[0]
		return fmt.Sprintf("**%s** 盘中%s `%+.2f%%` · 现价 %s", m.Symbol, m.verb(), m.ChangePct, trimNum(m.Last))
	}
	var b strings.Builder
	b.WriteString("**盘中异动**")
	for _, m := range sorted {
		// 列表里不再重复「现价」：抬头已经交代了这是一批行情。
		fmt.Fprintf(&b, "\n· **%s** %s `%+.2f%%` · %s", m.Symbol, m.verb(), m.ChangePct, trimNum(m.Last))
	}
	return b.String()
}

// trimNum prints a price the way money reads: two decimals, or three when the
// third carries something. 428.400 -> 428.40, 218.290 -> 218.29, 0.125 stays.
// Never one decimal — 428.4 next to 402.15 in a list looks truncated, and on a
// price it looks like a number that lost a digit.
func trimNum(v float64) string {
	s := strconv.FormatFloat(v, 'f', 3, 64)
	return strings.TrimSuffix(s, "0")
}
