package api

import (
	"context"
	"encoding/json"
	"net/http"
	"sync"
	"time"

	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/push"
)

// guardOfflinePush answers OpenIM's beforeOfflinePush webhook: whether to
// push this message to phones, to whom, and with what words.
//
// OpenIM 那边的语义（release-v3.8 的 internal/push/callback.go）：
//   - 回错误 → 这条不推。所以「不推」是 callbackDeny，不是回空名单：空名单在它
//     那里等于「照原来的」。webhooks.yml 里 failedContinue 必须是 false，否则错误
//     被当成「继续推」。
//   - userIDList 非空 → 换掉接收人名单。**单聊那条路径传的是 nil 指针，回名单会
//     让 openim-push 崩**，Decide 只在群消息里才给名单。
//   - offlinePushInfo 非空 → 整个换掉发送方自己填的那份。
//
// 5 秒超时，所以这里只查本地库和一个带缓存的群名。
func (s *Server) guardOfflinePush(w http.ResponseWriter, r *http.Request) {
	var req push.Request
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&req); err != nil {
		s.log.Warn("offline push: bad body", "err", err)
		callbackOK(w)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 3*time.Second)
	defer cancel()

	d := push.Decide(req, &pushNames{s: s, ctx: ctx}, bot.PlaceholderText)
	if d.Skip {
		s.log.Info("offline push: skip", "why", d.Reason, "from", req.SendID, "group", req.GroupID,
			"type", req.ContentType, "to", req.UserIDs)
		callbackDeny(w, "no offline push: "+d.Reason)
		return
	}
	resp := map[string]any{
		"actionCode": 0, "errCode": 0, "errMsg": "", "errDlt": "", "nextCode": 0,
		"offlinePushInfo": d.Info,
	}
	if d.UserIDs != nil {
		resp["userIDList"] = d.UserIDs
	}
	s.log.Info("offline push", "from", req.SendID, "group", req.GroupID, "type", req.ContentType,
		"to", firstNonNil(d.UserIDs, req.UserIDs), "title", d.Info.Title, "desc", trimForLog(d.Info.Desc))
	writeJSON(w, http.StatusOK, resp)
}

func firstNonNil(a, b []string) []string {
	if a != nil {
		return a
	}
	return b
}

// pushNames 给 push.Decide 查名字。昵称查本地库（名册就在那里，毫秒级）；群名要问
// OpenIM，加一分钟缓存——群里连着几条 @ 不该连着几次 RPC。
type pushNames struct {
	s   *Server
	ctx context.Context
}

func (n *pushNames) Nickname(userID string) string {
	u, err := n.s.store.GetUser(n.ctx, userID)
	if err != nil {
		return ""
	}
	return u.Nickname
}

func (n *pushNames) GroupName(groupID string) string {
	return n.s.groupNames.get(groupID, func() string {
		g, err := n.s.openim.GroupInfo(n.ctx, groupID)
		if err != nil {
			n.s.log.Warn("offline push: group name", "err", err, "group", groupID)
			return ""
		}
		return g.GroupName
	})
}

func (n *pushNames) Avatar(userID string) string {
	return n.s.faces.get(userID, func() string {
		face, err := n.s.openim.UserFace(n.ctx, userID)
		if err != nil {
			n.s.log.Warn("offline push: avatar", "err", err, "user", userID)
			return ""
		}
		return face
	})
}

func (n *pushNames) IsAgent(userID string) bool {
	for _, a := range n.s.cfg.Agents {
		if a.UserID == userID {
			return true
		}
	}
	return false
}

// nameCache 是一个带过期的 map。没查到（空串）也缓存，省得一个被解散的群每条
// 消息都去问一次。
type nameCache struct {
	mu  sync.Mutex
	ttl time.Duration
	m   map[string]cached
}

type cached struct {
	name string
	at   time.Time
}

func newNameCache(ttl time.Duration) *nameCache {
	return &nameCache{ttl: ttl, m: map[string]cached{}}
}

func (c *nameCache) get(key string, load func() string) string {
	c.mu.Lock()
	if v, ok := c.m[key]; ok && time.Since(v.at) < c.ttl {
		c.mu.Unlock()
		return v.name
	}
	c.mu.Unlock()
	name := load()
	c.mu.Lock()
	c.m[key] = cached{name: name, at: time.Now()}
	c.mu.Unlock()
	return name
}

// peek 只看缓存，不加载。
func (c *nameCache) peek(key string) (string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	v, ok := c.m[key]
	if !ok || time.Since(v.at) >= c.ttl {
		return "", false
	}
	return v.name, true
}

func (c *nameCache) put(key, name string) {
	c.mu.Lock()
	c.m[key] = cached{name: name, at: time.Now()}
	c.mu.Unlock()
}
