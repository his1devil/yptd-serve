package api

import (
	"fmt"
	"net/http"
	"strings"

	"github.com/his1devil/yptd/server/internal/config"
	"github.com/his1devil/yptd/server/internal/openim"
)

// handleRemoveAgent takes an agent out of a channel: DELETE /v1/groups/{group}/agents/{id}.
//
// 为什么不让客户端直接调 OpenIM 的踢人：OpenIM 只认群角色——群主谁都能踢，管理员只能踢
// 普通成员，普通成员谁都踢不了；而线上所有群里一个管理员都没有，于是每个群只有群主一个人
// 动得了 agent。可 agent 不是人：把一个吵人的机器人请出去，不该比把它请进来难。
//
// 规则（2026-09-19 定）：**群主，或者当初把这个 agent 拉进来的人**，可以移除它。邀请人
// OpenIM 的成员记录里本来就有（inviterUserID），不用另建表。这里验过资格之后用管理员身份
// 去踢——管理员 token 不受群角色限制。
//
// 顺手做两件客户端做不了的事：删掉这个群里给它配的盯盘/新闻（否则成了孤儿配置），并让
// agent 走之前在群里留一句话。App 现在不渲染 OpenIM 的成员变动通知，没有这句话，别人只会
// 发现 @ 不到它了，却不知道发生过什么。
func (s *Server) handleRemoveAgent(w http.ResponseWriter, r *http.Request) {
	cred, ok := s.authed(w, r)
	if !ok {
		return
	}
	groupID := strings.TrimSpace(r.PathValue("group"))
	agent, found := s.agentByID(r.PathValue("id"))
	if !found {
		fail(w, http.StatusNotFound, "no_such_agent", "没有这个 agent")
		return
	}
	members, err := s.openim.GroupMembers(r.Context(), groupID, []string{cred.UserID, agent.UserID})
	if err != nil {
		s.fail500(w, "group members", err)
		return
	}
	if allowed, why := mayRemoveAgent(cred.UserID, agent.UserID, members); !allowed {
		fail(w, http.StatusForbidden, "not_allowed", why)
		return
	}

	who := cred.UserID
	if u, err := s.store.GetUser(r.Context(), cred.UserID); err == nil && u.Nickname != "" {
		who = u.Nickname
	}
	// 先说话再走：走了就发不了了。发不出去不拦移除——留痕是体面，不是前提。
	text := fmt.Sprintf("%s 把我移出了这个频道。想让我回来，再邀请一次就行。", who)
	if _, err := s.openim.SendText(r.Context(), agent.UserID, agent.Nickname, "", groupID, text, ""); err != nil {
		s.log.Warn("remove agent: farewell", "err", err, "group", groupID, "agent", agent.UserID)
	}
	if err := s.openim.KickGroupMember(r.Context(), groupID, []string{agent.UserID}, "removed by "+cred.UserID); err != nil {
		s.fail500(w, "kick agent", err)
		return
	}
	if err := s.store.DeleteAgentConfig(r.Context(), agent.UserID, groupID); err != nil {
		s.log.Warn("remove agent: config", "err", err, "group", groupID, "agent", agent.UserID)
	}
	s.log.Info("agent removed", "group", groupID, "agent", agent.UserID, "by", cred.UserID)
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// mayRemoveAgent 是纯判断：群主，或者把这个 agent 拉进来的人。
func mayRemoveAgent(requester, agentID string, members []openim.Member) (bool, string) {
	var me, bot *openim.Member
	for i := range members {
		switch members[i].UserID {
		case requester:
			me = &members[i]
		case agentID:
			bot = &members[i]
		}
	}
	switch {
	case me == nil:
		return false, "你不在这个频道里"
	case bot == nil:
		return false, "它不在这个频道里"
	case me.RoleLevel == openim.RoleOwner:
		return true, ""
	case bot.InviterUserID != "" && bot.InviterUserID == requester:
		return true, ""
	}
	return false, "只有频道创建者，或者当初邀请它进来的人，可以移除它"
}

// isMember 回答请求者在不在这个群里。查不到当不在：宁可让人重试，也不放过一次越权。
func (s *Server) isMember(r *http.Request, groupID, userID string) bool {
	members, err := s.openim.GroupMembers(r.Context(), groupID, []string{userID})
	if err != nil {
		s.log.Warn("membership check", "err", err, "group", groupID, "user", userID)
		return false
	}
	for _, m := range members {
		if m.UserID == userID {
			return true
		}
	}
	return false
}

func (s *Server) agentByID(id string) (config.Agent, bool) {
	for _, a := range s.cfg.Agents {
		if a.UserID == id {
			return a, true
		}
	}
	return config.Agent{}, false
}
