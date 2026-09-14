package api

import (
	"context"
	"encoding/json"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/channel"
	"github.com/his1devil/yptd/server/internal/config"
)

// OpenIM posts to <configured url>/<command>, so these paths are the command
// names verbatim rather than something tidier.
const (
	afterSendGroupMsg  = "callbackAfterSendGroupMsgCommand"
	afterSendSingleMsg = "callbackAfterSendSingleMsgCommand"
	// 两条把人放进群的路，都要拦。只拦上面那条的话，换成「建个新群，成员里带上他」
	// 一样能把人拖进来。
	beforeInvite     = "callbackBeforeInviteJoinGroupCommand"
	beforeMembersAdd = "callbackBeforeMembersJoinGroupCommand"
	// 自己申请加入一个频道，走的是另一条路，也要看频道自己的开关
	beforeSelfJoin = "callbackBeforeJoinGroupCommand"
)

// callbackReq is the part of OpenIM's payload this service reads. The
// callback carries `atUserList` already parsed, which is what decides whether
// the bot was addressed -- no text matching needed.
type callbackReq struct {
	ClientMsgID    string   `json:"clientMsgID"`
	SendID         string   `json:"sendID"`
	RecvID         string   `json:"recvID"`
	GroupID        string   `json:"groupID"`
	SenderNickname string   `json:"senderNickname"`
	ContentType    int32    `json:"contentType"`
	Content        string   `json:"content"`
	AtUserList     []string `json:"atUserList"`
	Ex             string   `json:"ex"`
}

// handleCallback answers OpenIM's webhook.
//
// It must return within OpenIM's five-second callback timeout, so the work
// happens on its own goroutine and the response goes out immediately. A
// webhook is a notification, not a request for an answer.
func (s *Server) handleCallback(w http.ResponseWriter, r *http.Request) {
	command := strings.TrimPrefix(r.URL.Path, "/callback/")

	// 把人拉进群的两个 before 钩子要当场答复：它们是否决权，不是通知。
	// 也必须在下面 bot == nil 那道门之前——没有 agent 运行时的部署，隐私开关照样得管用。
	if command == beforeInvite || command == beforeMembersAdd {
		s.guardJoin(w, r, command)
		return
	}
	if command == beforeSelfJoin {
		s.guardSelfJoin(w, r)
		return
	}

	var req callbackReq
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&req); err != nil {
		s.log.Warn("callback: bad body", "command", command, "err", err)
		callbackOK(w)
		return
	}
	if s.bot == nil {
		callbackOK(w)
		return
	}

	text, inContent := bot.ParseContent(req.ContentType, req.Content, s.bot.Nicknames())
	addressed := addressedAgents(req, inContent, s.bot.Agents(), command)
	base := bot.Message{
		ClientMsgID:    req.ClientMsgID,
		SenderID:       req.SendID,
		SenderNickname: req.SenderNickname,
		GroupID:        req.GroupID,
		ConversationID: conversationID(req),
		ContentType:    req.ContentType,
		Text:           text,
		Attachments:    bot.ParseAttachments(req.Ex),
	}

	// One message may call on several agents, and each answers for itself.
	// They queue behind one another per conversation rather than talking at
	// the same time.
	var handling []string
	for _, agentID := range addressed {
		message := base
		message.AgentID = agentID
		if !s.bot.Wants(message) {
			continue
		}
		handling = append(handling, agentID)
		go func(m bot.Message) {
			// Its own context: the request's is cancelled the moment this
			// handler returns, and the agent takes far longer than that.
			ctx, cancel := context.WithTimeout(context.Background(), s.botBudget)
			defer cancel()
			s.bot.Handle(ctx, m)
		}(message)
	}

	// One line per addressed-looking message, because the alternative when
	// the bot stays silent is guessing which of five conditions rejected it.
	s.log.Info("callback",
		"command", command, "from", req.SendID, "group", req.GroupID,
		"recv", req.RecvID, "type", req.ContentType, "at", req.AtUserList,
		"addressed", addressed, "text", trimForLog(text),
		"handling", handling)
	callbackOK(w)
}

func callbackOK(w http.ResponseWriter) {
	writeJSON(w, http.StatusOK, map[string]any{"errCode": 0, "errMsg": "", "errDlt": ""})
}

// callbackDeny refuses the operation.
//
// The three fields are all load-bearing and the naming is a trap. OpenIM turns
// a response into an error in CommonCallbackResp.Parse:
//
//	if c.ActionCode == NoError && c.NextCode == Next { return error(errCode, errMsg) }
//
// where NoError is 0 and Next is 1. So a refusal is actionCode 0 **and**
// nextCode 1 — errCode alone does nothing, and a response carrying only
// errCode sails straight through as permission granted. Reading it as
// "nextCode: 1 means carry on" gets it exactly backwards.
func callbackDeny(w http.ResponseWriter, msg string) {
	// errDlt 留空：OpenIM 把 errMsg 和 errDlt 拼在一起当最终文案
	// （errs.NewCodeError(code, msg).WithDetail(dlt)），两处填同一句话会读到两遍。
	writeJSON(w, http.StatusOK, map[string]any{
		"actionCode": 0, "nextCode": 1,
		"errCode": 10001, "errMsg": msg, "errDlt": "",
	})
}

// joinReq covers both shapes: the invite callback names the people directly,
// the create-group one wraps them in member objects.
type joinReq struct {
	GroupID        string   `json:"groupID"`
	InvitedUserIDs []string `json:"invitedUserIDs"`
	MemberList     []struct {
		UserID string `json:"userID"`
	} `json:"memberList"`
}

// userIDs is who this request would put in the group, minus anyone who is not
// being put there by someone else.
//
// For the create-group callback that means dropping the first entry, and the
// rule behind that is worth stating plainly because it is not a special case:
//
//	在 beforeMembersJoinGroup 这个回调里，列表第一个永远是「自己把自己放进去的
//	那个人」——建群时是群主，自助入群时是申请人（那种情况列表就他一个）。
//	别人把你放进去走的是 beforeInviteUserToGroup，是另一条路。
//
// So the first entry is never someone being added by somebody else, and this
// switch has nothing to say about them. Without the skip, nobody could create
// a group until they had allowed other people to add them to groups — refused
// entry to your own room — and nobody could walk into an open channel because
// they had declined to be dragged into other ones. Those are different things.
//
// The payload carries no role or operator id, so the position is all there is
// to go on. If upstream ever reorders that list the failure is loud, not
// silent: group creation starts getting refused for the creator.
func (j joinReq) userIDs(command string) []string {
	if len(j.InvitedUserIDs) > 0 {
		return j.InvitedUserIDs
	}
	members := j.MemberList
	if command == beforeMembersAdd && len(members) > 0 {
		members = members[1:]
	}
	out := make([]string, 0, len(members))
	for _, m := range members {
		if m.UserID != "" {
			out = append(out, m.UserID)
		}
	}
	return out
}

// guardSelfJoin refuses a join request to a channel that is not open.
//
// Hiding the join button is a request; this is the rule. OpenIM asks before it
// looks at anything else about the request, so a client that calls join_group
// on a closed channel is turned away here whatever it believes.
//
// It reads the channel's own switch, which lives in the group's `ex`. The
// person's own "don't add me to groups" switch is deliberately NOT consulted:
// walking into a room yourself is not the same as being dragged into one.
func (s *Server) guardSelfJoin(w http.ResponseWriter, r *http.Request) {
	var req struct {
		GroupID string `json:"groupID"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&req); err != nil || req.GroupID == "" {
		s.log.Warn("callback: bad self-join body", "err", err)
		callbackOK(w)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 3*time.Second)
	defer cancel()

	g, err := s.openim.GroupInfo(ctx, req.GroupID)
	if err != nil {
		// 查不到就放行，和上面那道门同一个理由：这是用来挡「明确说了不开放」的，
		// 不是用来挡所有查不清的情况。
		s.log.Warn("callback: self-join lookup", "err", err, "group", req.GroupID)
		callbackOK(w)
		return
	}
	if channel.Parse(g.Ex).Joinable {
		callbackOK(w)
		return
	}
	s.log.Info("callback: self-join refused", "group", req.GroupID, "name", g.GroupName)
	callbackDeny(w, "这个频道没有开放自由加入，请让频道里的人邀请你")
}

// refusedBy names the people in ids who have not opened themselves to being
// added to groups. Empty means the batch may go through.
//
// Pure, with the lookup handed in: who may be added is a policy question and
// wants to be readable and testable on its own, not tangled with an HTTP
// handler and a database.
//
// `lookup` reports ok=false when it cannot say — no such account, or the
// database did not answer. Those pass. This gate exists to honour people who
// said no, not to block everything it is unsure about.
func refusedBy(ids []string, isAgent func(string) bool, lookup func(string) (nickname string, joinable, ok bool)) []string {
	var refused []string
	seen := map[string]bool{}
	for _, id := range ids {
		if id == "" || seen[id] || isAgent(id) {
			// agent 是被派来干活的，没有「愿不愿意进群」这回事
			continue
		}
		seen[id] = true
		nickname, joinable, ok := lookup(id)
		if !ok || joinable {
			continue
		}
		if nickname == "" {
			nickname = id
		}
		refused = append(refused, nickname)
	}
	return refused
}

// guardJoin refuses to let anyone be put in a group who has not allowed it.
//
// This is where that switch is actually enforced. Clients create groups and
// invite people by talking to OpenIM directly with their own token, so a flag
// only this service consulted would be a request, not a rule; OpenIM asking
// first is what makes it one.
//
// It is all-or-nothing by necessity. OpenIM's response type has a
// RefusedMembersAccount field, but the code that would act on it is commented
// out upstream, so refusing one person refuses the whole batch. Clients are
// expected to leave the unwilling out of the request in the first place —
// they can see the flag — and this is the backstop for the ones that do not.
func (s *Server) guardJoin(w http.ResponseWriter, r *http.Request, command string) {
	var req joinReq
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&req); err != nil {
		// 读不懂的请求不该变成一道门禁：放行，记一条。
		s.log.Warn("callback: bad join body", "command", command, "err", err)
		callbackOK(w)
		return
	}
	ids := req.userIDs(command)
	if len(ids) == 0 {
		callbackOK(w)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 3*time.Second)
	defer cancel()

	agents := map[string]bool{}
	for _, a := range s.cfg.Agents {
		agents[a.UserID] = true
	}
	refused := refusedBy(ids,
		func(id string) bool { return agents[id] },
		func(id string) (string, bool, bool) {
			u, err := s.store.GetUser(ctx, id)
			if err != nil {
				s.log.Warn("callback: join guard lookup", "err", err, "user", id)
				return "", false, false
			}
			return u.Nickname, u.Joinable, true
		})
	if len(refused) == 0 {
		callbackOK(w)
		return
	}
	s.log.Info("callback: join refused", "command", command, "group", req.GroupID, "who", strings.Join(refused, " "))
	callbackDeny(w, strings.Join(refused, "、")+" 没有开放被加入群聊")
}

// addressedAgents: in a group an agent must be mentioned; in a direct chat
// the message being sent to it is the whole of being addressed.
//
// Returned in roster order rather than mention order, so that when someone
// calls on two agents at once the group always hears them in the same
// sequence.
func addressedAgents(req callbackReq, inContent []string, agents []config.Agent, command string) []string {
	if command == afterSendSingleMsg || req.GroupID == "" {
		for _, a := range agents {
			if a.UserID == req.RecvID {
				return []string{a.UserID}
			}
		}
		return nil
	}
	// Two places carry the mentions and neither is always filled: the
	// callback's own field is what the SDK sends, and the copy inside the
	// message element is what the REST API sends. Reading only one means
	// silently ignoring half the ways a message can arrive.
	mentioned := make(map[string]bool, len(req.AtUserList)+len(inContent))
	for _, id := range req.AtUserList {
		mentioned[id] = true
	}
	for _, id := range inContent {
		mentioned[id] = true
	}
	var out []string
	for _, a := range agents {
		if mentioned[a.UserID] {
			out = append(out, a.UserID)
		}
	}
	return out
}

// conversationID rebuilds the id OpenIM uses, which a revoke needs.
func conversationID(req callbackReq) string {
	if req.GroupID != "" {
		return "sg_" + req.GroupID
	}
	pair := []string{req.SendID, req.RecvID}
	sort.Strings(pair)
	return "si_" + pair[0] + "_" + pair[1]
}

func trimForLog(value string) string {
	if len(value) > 80 {
		return value[:80] + "…"
	}
	return value
}

// botBudget leaves room for the agent's own timeout plus the round trips
// around it.
func botBudget(agent time.Duration) time.Duration { return agent + time.Minute }
