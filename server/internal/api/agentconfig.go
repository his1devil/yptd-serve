package api

import (
	"context"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/his1devil/yptd/server/internal/config"
	"github.com/his1devil/yptd/server/internal/store"
)

// 表单由服务端描述，客户端按 type 通用渲染。以后加新 agent 不用动客户端。
//
// 代价是通用表单写不出针对性的解释，所以每个字段都带一句 hint——「为什么是这个值」
// 的知识在服务端，不在界面代码里。一个配 1% 阈值的人需要有人告诉他那会很吵。
type field struct {
	Key   string `json:"key"`
	Type  string `json:"type"` // symbols | number | duration | text
	Label string `json:"label"`
	Hint  string `json:"hint,omitempty"`
	Unit  string `json:"unit,omitempty"`
	Min   any    `json:"min,omitempty"`
	Max   any    `json:"max,omitempty"`
	Step  any    `json:"step,omitempty"`
}

type form struct {
	Kind   string  `json:"kind"`
	Title  string  `json:"title"`
	Note   string  `json:"note,omitempty"`
	Fields []field `json:"fields"`
}

var watchForm = form{
	Kind:  "watch",
	Title: "盯盘",
	Note:  "只在这个频道生效。同一个 agent 在别的频道可以盯别的标的。",
	Fields: []field{
		{Key: "symbols", Type: "symbols", Label: "盯哪些标的",
			Hint: "长桥的代码格式：700.HK、NVDA.US、TSLA.US。留空就是不盯。"},
		{Key: "threshold", Type: "number", Label: "涨跌多少算异动", Unit: "%",
			Min: 0.5, Max: 50, Step: 0.5,
			Hint: "3% 大概是一天几条。调到 1% 会很吵，调到 10% 基本只在大事时开口。"},
		{Key: "every", Type: "duration", Label: "多久看一次",
			Min: "30s", Hint: "行情是一次进程调用，太密没有意义；默认两分钟。"},
	},
}

var newsForm = form{
	Kind:  "news",
	Title: "热点推送",
	Note:  "只在这个频道生效。",
	Fields: []field{
		{Key: "batch", Type: "number", Label: "攒够几条再推", Min: 1, Max: 20, Step: 1,
			Hint: "攒着一起发比来一条推一条安静得多。默认 3 条。"},
		{Key: "within", Type: "duration", Label: "最长等多久",
			Hint: "没攒够也别一直等。慢的那天不该一条都听不到，默认 30 分钟。"},
		{Key: "categories", Type: "text", Label: "只要这些分类",
			Hint: "留空就是全都要。多个用空格分开，例如 ai-models。"},
	},
}

func formFor(a config.Agent) (form, bool) {
	// 有什么能力看的是名册里配过什么。HALX 这类没有推送能力的 agent 就没有配置页。
	switch {
	case a.News.Feed != "":
		return newsForm, true
	case len(a.Watch.Symbols) > 0 || a.UserID == "agentcharlie":
		return watchForm, true
	}
	return form{}, false
}

// handleAgentConfig reads or writes one agent's settings inside one group.
//
//	GET  /v1/agents/{id}/config?group=<groupID>
//	PUT  /v1/agents/{id}/config?group=<groupID>
//
// 谁都能改：进得来就是自己人，和邀请码一个道理。但改完会往那个群里发一条消息——
// 谁都能改的东西，最怕的不是有人捣乱，是改了没人知道；突然不推了或者推得太吵，
// 群里的人能翻上去看到是谁什么时候改的。
func (s *Server) handleAgentConfig(w http.ResponseWriter, r *http.Request) {
	cred, ok := s.authed(w, r)
	if !ok {
		return
	}
	agentID := r.PathValue("id")
	groupID := strings.TrimSpace(r.URL.Query().Get("group"))
	if groupID == "" {
		fail(w, http.StatusBadRequest, "missing_group", "要说明是哪个频道")
		return
	}
	var agent config.Agent
	found := false
	for _, a := range s.cfg.Agents {
		if a.UserID == agentID {
			agent, found = a, true
			break
		}
	}
	if !found {
		fail(w, http.StatusNotFound, "no_such_agent", "没有这个 agent")
		return
	}
	shape, configurable := formFor(agent)
	if !configurable {
		fail(w, http.StatusNotFound, "not_configurable", "这个 agent 没有可配置的项")
		return
	}

	if r.Method == http.MethodGet {
		s.readAgentConfig(w, r, agent, groupID, shape)
		return
	}
	s.writeAgentConfig(w, r, cred, agent, groupID, shape)
}

func (s *Server) readAgentConfig(w http.ResponseWriter, r *http.Request, agent config.Agent, groupID string, shape form) {
	row, ok, err := s.store.AgentConfig(r.Context(), agent.UserID, groupID)
	if err != nil {
		s.fail500(w, "agent config", err)
		return
	}
	out := map[string]any{"form": shape, "configured": ok}
	if ok && row.UpdatedBy != "" {
		out["updated_by"] = row.UpdatedBy
		out["updated_at"] = row.UpdatedAt.UnixMilli()
	}
	// 没配过就把名册里的默认值给出去，界面上是填好的起点而不是一片空白
	switch shape.Kind {
	case "watch":
		wv := agent.Watch
		if ok && row.Watch != nil {
			wv = *row.Watch
		}
		out["value"] = wv.Tuned()
	case "news":
		nv := agent.News
		if ok && row.News != nil {
			nv = *row.News
		}
		out["value"] = nv.Tuned()
	}
	writeJSON(w, http.StatusOK, out)
}

func (s *Server) writeAgentConfig(w http.ResponseWriter, r *http.Request, cred store.Credential, agent config.Agent, groupID string, shape form) {
	row := store.GroupAgent{AgentID: agent.UserID, GroupID: groupID, UpdatedBy: cred.UserID}
	var summary string
	switch shape.Kind {
	case "watch":
		var in config.Watch
		if !decode(w, r, &in) {
			return
		}
		in.Symbols = cleanSymbols(in.Symbols)
		if err := checkWatch(in); err != "" {
			fail(w, http.StatusBadRequest, "bad_config", err)
			return
		}
		tuned := in.Tuned()
		row.Watch = &tuned
		summary = watchSummary(tuned)
	case "news":
		var in config.News
		if !decode(w, r, &in) {
			return
		}
		// feed 不让客户端改：它决定连哪个上游，不是口味问题
		in.Feed = agent.News.Feed
		tuned := in.Tuned()
		row.News = &tuned
		summary = newsSummary(tuned)
	}
	if err := s.store.SetAgentConfig(r.Context(), row); err != nil {
		s.fail500(w, "set agent config", err)
		return
	}
	// 留痕：往群里说一声谁改了什么。失败不影响配置本身已经写好了。
	go s.announceConfig(agent, groupID, cred.UserID, shape.Title, summary)
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// cleanSymbols 去掉空白和重复，并统一成大写——长桥的代码是大写的，
// 人手打的 nvda.us 不该变成一个查不到的标的。
func cleanSymbols(in []string) []string {
	seen := map[string]bool{}
	out := make([]string, 0, len(in))
	for _, s := range in {
		s = strings.ToUpper(strings.TrimSpace(s))
		if s == "" || seen[s] {
			continue
		}
		seen[s] = true
		out = append(out, s)
	}
	return out
}

func checkWatch(w config.Watch) string {
	if len(w.Symbols) > 50 {
		return "最多盯 50 个标的"
	}
	for _, s := range w.Symbols {
		// 长桥的形式是 <代码>.<市场>。不拦死，只拦明显不对的，免得挡住我们不知道的市场。
		if !strings.Contains(s, ".") {
			return s + " 看着不像长桥的代码，应该是 700.HK、NVDA.US 这样"
		}
	}
	if w.Threshold < 0 || w.Threshold > 50 {
		return "阈值要在 0 到 50 之间"
	}
	if w.Every != "" {
		if _, err := time.ParseDuration(w.Every); err != nil {
			return "看一次的间隔写法不对，像 2m、30s 这样"
		}
	}
	return ""
}

func watchSummary(w config.Watch) string {
	if len(w.Symbols) == 0 {
		return "不盯任何标的"
	}
	return strings.Join(w.Symbols, " ") + "，异动阈值 " + trimPct(w.Threshold) + "%，每 " + w.Every + " 看一次"
}

func newsSummary(n config.News) string {
	s := "攒够 " + itoa(n.Batch) + " 条或等 " + n.Within + " 推一次"
	if len(n.Categories) > 0 {
		s += "，只要 " + strings.Join(n.Categories, " ")
	}
	return s
}

// announceConfig tells the room who changed what.
//
// 单独一条消息，不是日志：突然不推了或者推得太吵时，群里的人能翻上去看到是谁什么
// 时候改的，比去翻服务器日志现实得多。这条消息本身也是配置的可读快照。
func (s *Server) announceConfig(agent config.Agent, groupID, byUser, title, summary string) {
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
	defer cancel()
	who := byUser
	if u, err := s.store.GetUser(ctx, byUser); err == nil && u.Nickname != "" {
		who = u.Nickname
	}
	text := who + " 把" + title + "改成了：" + summary
	if _, err := s.openim.SendText(ctx, agent.UserID, agent.Nickname, "", groupID, text, ""); err != nil {
		// 配置已经写好了，这条没发出去只是少了个记录
		s.log.Warn("agent config: announce", "err", err, "group", groupID)
	}
}

func trimPct(v float64) string {
	out := strconv.FormatFloat(v, 'f', 2, 64)
	out = strings.TrimRight(out, "0")
	return strings.TrimSuffix(out, ".")
}

func itoa(n int) string { return strconv.Itoa(n) }
