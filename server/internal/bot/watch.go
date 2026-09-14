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
	rooms  Rooms
	quotes Quotes
	send   Sender
	groups Groups
	log    *slog.Logger

	mu sync.Mutex
	// seen is the last percentage actually announced, **per room per symbol**.
	// 按群分是必须的：A 群报过 NVDA 之后，B 群不该因此哑掉——两个群各看各的，
	// 阈值也可能不一样。合成一张表的话，谁先轮到谁说话，剩下的群永远慢一步。
	seen map[string]float64
	// lastPoll is when each room was last actually asked about. Rooms carry
	// their own interval, so a room set to 30s must not drag every other
	// room's polling up to 30s with it.
	lastPoll map[string]time.Time
}

// Rooms answers what this agent has been configured to watch, per group.
//
// 每轮重读，而不是启动时读一次：配置是在 app 里改的，改完下一轮就生效，不用重启，
// 也不用管 goroutine 的生死。
//
// 只返回「配过的」，默认值的回退交给调用方：这样 poll 能先花一次便宜的库查询问出
// 「这个 agent 到底有没有事做」，没事做就连 OpenIM 都不用问——名册里五个 agent，
// 只有一两个会盯盘，其余的不该每 30 秒去问一次自己在哪些群。
type Rooms interface {
	WatchFor(ctx context.Context, agentID string) (map[string]config.Watch, error)
}

// RoomWatch is one group's watchlist.
type RoomWatch struct {
	GroupID string
	Watch   config.Watch
}

// Groups answers where an agent can currently speak. Read every poll rather
// than cached: an agent joins and leaves rooms without this service hearing.
type Groups interface {
	JoinedGroups(ctx context.Context, userID string) ([]string, error)
}

func NewWatcher(agent config.Agent, rooms Rooms, quotes Quotes, send Sender, groups Groups, log *slog.Logger) *Watcher {
	if log == nil {
		log = slog.Default()
	}
	return &Watcher{
		agent:    agent,
		rooms:    rooms,
		quotes:   quotes,
		send:     send,
		groups:   groups,
		log:      log,
		seen:     map[string]float64{},
		lastPoll: map[string]time.Time{},
	}
}

/** 一条 seen 记录的键：群 + 标的 */
func seenKey(groupID, symbol string) string { return groupID + "\x00" + symbol }

// Run polls until ctx is done.
//
// 固定节拍转，每轮再按各群自己的间隔决定要不要真去问。用「所有群里最短的间隔」当
// 节拍的话，一个群配了 30 秒就会把整个 agent 的频率拖上去。
func (w *Watcher) Run(ctx context.Context) {
	if w.rooms == nil {
		return
	}
	w.log.Info("watch: started", "agent", w.agent.UserID, "tick", TICK)
	t := time.NewTicker(TICK)
	defer t.Stop()
	for {
		w.poll(ctx)
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
	}
}

// TICK 是轮询的节拍。各群自己的 every 决定它这一轮要不要真被问，所以这个值只需要
// 比最短的 every 更细就行。
const TICK = 30 * time.Second

func (w *Watcher) poll(ctx context.Context) {
	byGroup, err := w.rooms.WatchFor(ctx, w.agent.UserID)
	if err != nil {
		w.log.Warn("watch: rooms", "err", err, "agent", w.agent.UserID)
		return
	}
	// 一个群都没配过、名册里也没有默认清单：这个 agent 没有盯盘这回事，
	// 到此为止，连「我在哪些群」都不用问。
	if len(byGroup) == 0 && !w.agent.Watch.On() {
		return
	}
	joined, err := w.groups.JoinedGroups(ctx, w.agent.UserID)
	if err != nil {
		w.log.Warn("watch: joined groups", "err", err, "agent", w.agent.UserID)
		return
	}
	rooms := make([]RoomWatch, 0, len(joined))
	for _, g := range joined {
		cfg, configured := byGroup[g]
		if !configured {
			// 这个群没配过：用名册里的默认值，但要尊重它自己的 Groups 白名单
			if len(w.agent.Watch.Rooms([]string{g})) == 0 {
				continue
			}
			cfg = w.agent.Watch
		}
		rooms = append(rooms, RoomWatch{GroupID: g, Watch: cfg})
	}

	// 只问这一轮真正到点的群
	now := time.Now()
	due := make([]RoomWatch, 0, len(rooms))
	for _, r := range rooms {
		if !r.Watch.On() {
			continue
		}
		last, ok := w.lastPoll[r.GroupID]
		if ok && now.Sub(last) < r.Watch.Interval() {
			continue
		}
		due = append(due, r)
	}
	if len(due) == 0 {
		return
	}

	// 几个群盯同一个标的时只拉一次：行情 CLI 是一次进程启动，不该按群数翻倍
	wanted := map[string]bool{}
	for _, r := range due {
		for _, sym := range r.Watch.Tuned().Symbols {
			wanted[sym] = true
		}
	}
	symbols := make([]string, 0, len(wanted))
	for sym := range wanted {
		symbols = append(symbols, sym)
	}
	sort.Strings(symbols)

	quotes, err := w.quotes.Quotes(ctx, symbols)
	if err != nil {
		// 每轮都同样失败是 CLI 登录过期的样子。记一条，不推任何东西：
		// 数字不知道的时候，沉默才是对的输出。
		w.log.Warn("watch: quotes", "err", err, "agent", w.agent.UserID)
		return
	}
	byID := make(map[string]Quote, len(quotes))
	for _, q := range quotes {
		byID[q.Symbol] = q
	}

	for _, r := range due {
		w.lastPoll[r.GroupID] = now
		tuned := r.Watch.Tuned()
		mine := make([]Quote, 0, len(tuned.Symbols))
		for _, sym := range tuned.Symbols {
			if q, ok := byID[sym]; ok {
				mine = append(mine, q)
			}
		}
		moves := w.decide(r.GroupID, tuned.Threshold, mine)
		if len(moves) == 0 {
			continue
		}
		text := Announce(moves)
		if _, err := w.send.SendText(ctx, w.agent.UserID, w.agent.Nickname, "", r.GroupID, text, ""); err != nil {
			w.log.Error("watch: send", "err", err, "agent", w.agent.UserID, "group", r.GroupID)
			w.forget(r.GroupID, moves)
			continue
		}
		w.log.Info("watch: announced", "agent", w.agent.UserID, "group", r.GroupID, "symbols", len(moves))
	}
}

// decide picks the quotes worth announcing in one room and records them.
func (w *Watcher) decide(groupID string, threshold float64, quotes []Quote) []Moved {
	w.mu.Lock()
	defer w.mu.Unlock()
	var due []Moved
	for _, q := range quotes {
		if q.Halted() {
			continue
		}
		key := seenKey(groupID, q.Symbol)
		prev, had := w.seen[key]
		m := Worth(q.ChangePct, prev, had, threshold)
		if m.Keep() {
			w.seen[key] = q.ChangePct
		} else {
			delete(w.seen, key)
		}
		if m.Say() {
			due = append(due, Moved{Quote: q, Move: m})
		}
	}
	return due
}

// forget undoes decide's bookkeeping for moves that never made it out.
func (w *Watcher) forget(groupID string, moves []Moved) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, m := range moves {
		delete(w.seen, seenKey(groupID, m.Symbol))
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
