// Package openim wraps the few OpenIM admin endpoints yptd-server needs.
//
// Only this package holds the OpenIM secret. Everything above it deals in
// yptd's own identities and asks here when an OpenIM token is required.
package openim

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"sync"
	"time"

	"github.com/his1devil/yptd/server/internal/push"
)

type Client struct {
	base    string
	secret  string
	adminID string
	http    *http.Client

	// PublicAPI is openim-api's address as clients reach it, e.g.
	// https://im.example.com. OpenIM builds the object URLs it hands back from
	// the request's own host, and this service talks to it over loopback — so
	// without this an uploaded file comes back as http://127.0.0.1:10002/object/…,
	// which no phone can open. Sent as X-Request-Api, the same header nginx sets.
	PublicAPI string

	// The admin token is valid for 90 days but costs a round trip, so it is
	// cached and refreshed well before OpenIM would expire it.
	mu         sync.Mutex
	adminToken string
	adminUntil time.Time
}

func New(base, secret, adminUserID string) *Client {
	return &Client{
		base:    base,
		secret:  secret,
		adminID: adminUserID,
		http:    &http.Client{Timeout: 20 * time.Second},
	}
}

// OpenIM error codes we branch on. The rest stay anonymous: a caller that
// cannot do anything specific about a code should not pretend to know it.
const (
	// CodeRegisteredAlready is /user/user_register's answer when the userID
	// exists. It is the one register failure a caller can safely continue past.
	CodeRegisteredAlready = 1102
)

// Error is a non-zero errCode from OpenIM, kept structured so callers can
// distinguish "already registered" from "server is down".
type Error struct {
	Code   int
	Msg    string
	Detail string
	Path   string
}

func (e *Error) Error() string {
	if e.Detail != "" {
		return fmt.Sprintf("openim %s: %d %s (%s)", e.Path, e.Code, e.Msg, e.Detail)
	}
	return fmt.Sprintf("openim %s: %d %s", e.Path, e.Code, e.Msg)
}

type envelope struct {
	ErrCode int             `json:"errCode"`
	ErrMsg  string          `json:"errMsg"`
	ErrDlt  string          `json:"errDlt"`
	Data    json.RawMessage `json:"data"`
}

func (c *Client) post(ctx context.Context, path string, body any, token string, out any) error {
	payload, err := json.Marshal(body)
	if err != nil {
		return fmt.Errorf("openim %s: encode: %w", path, err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.base+path, bytes.NewReader(payload))
	if err != nil {
		return fmt.Errorf("openim %s: request: %w", path, err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("operationID", operationID())
	if c.PublicAPI != "" {
		req.Header.Set("X-Request-Api", c.PublicAPI)
	}
	if token != "" {
		req.Header.Set("token", token)
	}

	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("openim %s: %w", path, err)
	}
	defer resp.Body.Close()

	var env envelope
	if err := json.NewDecoder(resp.Body).Decode(&env); err != nil {
		return fmt.Errorf("openim %s: decode (http %d): %w", path, resp.StatusCode, err)
	}
	if env.ErrCode != 0 {
		return &Error{Code: env.ErrCode, Msg: env.ErrMsg, Detail: env.ErrDlt, Path: path}
	}
	if out != nil && len(env.Data) > 0 {
		if err := json.Unmarshal(env.Data, out); err != nil {
			return fmt.Errorf("openim %s: decode data: %w", path, err)
		}
	}
	return nil
}

func operationID() string {
	return "yptd-" + strconv.FormatInt(time.Now().UnixNano(), 36)
}

// AdminToken returns a cached admin token, minting a new one when needed.
func (c *Client) AdminToken(ctx context.Context) (string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.adminToken != "" && time.Now().Before(c.adminUntil) {
		return c.adminToken, nil
	}

	var out struct {
		Token string `json:"token"`
	}
	body := map[string]any{"secret": c.secret, "userID": c.adminID}
	if err := c.post(ctx, "/auth/get_admin_token", body, "", &out); err != nil {
		return "", err
	}
	if out.Token == "" {
		return "", fmt.Errorf("openim /auth/get_admin_token: empty token")
	}
	c.adminToken = out.Token
	// OpenIM's tokenPolicy.expire defaults to 90 days; refresh after one hour
	// so a policy change on the server cannot leave us holding a dead token.
	c.adminUntil = time.Now().Add(time.Hour)
	return out.Token, nil
}

// UserToken mints an OpenIM token for userID on the given platform.
func (c *Client) UserToken(ctx context.Context, userID string, platformID int) (string, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return "", err
	}
	var out struct {
		Token string `json:"token"`
	}
	body := map[string]any{"secret": c.secret, "userID": userID, "platformID": platformID}
	if err := c.post(ctx, "/auth/get_user_token", body, admin, &out); err != nil {
		return "", err
	}
	if out.Token == "" {
		return "", fmt.Errorf("openim /auth/get_user_token: empty token")
	}
	return out.Token, nil
}

// RegisterUser creates the OpenIM-side identity. Registering a userID that
// already exists is reported by OpenIM as an error; callers decide whether
// that is fatal.
func (c *Client) RegisterUser(ctx context.Context, userID, nickname, faceURL string) error {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return err
	}
	body := map[string]any{
		"users": []map[string]string{
			{"userID": userID, "nickname": nickname, "faceURL": faceURL},
		},
	}
	return c.post(ctx, "/user/user_register", body, admin, nil)
}

// UserExists reports whether OpenIM already knows this userID.
// UpdateUser changes an existing account's display name.
//
// OpenIM has no endpoint for deleting a user, so an account that outlives its
// purpose can only be renamed or left alone; this is also how the bot's
// nickname is kept in step with the configured one.
func (c *Client) UpdateUser(ctx context.Context, userID, nickname, faceURL string) error {
	token, err := c.AdminToken(ctx)
	if err != nil {
		return err
	}
	// OpenIM wants the fields nested; a flat body comes back as "UserInfo is empty".
	info := map[string]any{"userID": userID, "nickname": nickname}
	// 空串不发：update_user_info 是按字段覆盖的，发空会把已有头像抹掉。
	if faceURL != "" {
		info["faceURL"] = faceURL
	}
	body := map[string]any{"userInfo": info}
	return c.post(ctx, "/user/update_user_info", body, token, nil)
}

func (c *Client) UserExists(ctx context.Context, userID string) (bool, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return false, err
	}
	// `usersInfo`, not `usersStatus`: the latter is what the online-status
	// endpoint returns, and decoding into it silently yielded an empty list,
	// so every existing account looked missing.
	var out struct {
		UsersInfo []struct {
			UserID string `json:"userID"`
		} `json:"usersInfo"`
	}
	body := map[string]any{"userIDs": []string{userID}}
	if err := c.post(ctx, "/user/get_users_info", body, admin, &out); err != nil {
		// A lookup miss is not an error worth propagating as "unknown".
		var apiErr *Error
		if ok := asError(err, &apiErr); ok && apiErr.Code == 1004 {
			return false, nil
		}
		return false, err
	}
	for _, user := range out.UsersInfo {
		if user.UserID == userID {
			return true, nil
		}
	}
	return false, nil
}

// UserFaces reads many accounts' avatar URLs in one call. Accounts with none
// are simply absent from the result.
func (c *Client) UserFaces(ctx context.Context, userIDs []string) (map[string]string, error) {
	out := map[string]string{}
	if len(userIDs) == 0 {
		return out, nil
	}
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return nil, err
	}
	var resp struct {
		UsersInfo []struct {
			UserID  string `json:"userID"`
			FaceURL string `json:"faceURL"`
		} `json:"usersInfo"`
	}
	if err := c.post(ctx, "/user/get_users_info", map[string]any{"userIDs": userIDs}, admin, &resp); err != nil {
		return nil, err
	}
	for _, u := range resp.UsersInfo {
		if u.FaceURL != "" {
			out[u.UserID] = u.FaceURL
		}
	}
	return out, nil
}

// UserFace reads one account's avatar URL. Empty when the account has none,
// which is the common case — most people never set one.
//
// yptd 自己的库里没有头像字段，头像只存在 OpenIM 的 faceURL 上。推送通知要用它。
func (c *Client) UserFace(ctx context.Context, userID string) (string, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return "", err
	}
	var out struct {
		UsersInfo []struct {
			UserID  string `json:"userID"`
			FaceURL string `json:"faceURL"`
		} `json:"usersInfo"`
	}
	if err := c.post(ctx, "/user/get_users_info", map[string]any{"userIDs": []string{userID}}, admin, &out); err != nil {
		return "", err
	}
	for _, u := range out.UsersInfo {
		if u.UserID == userID {
			return u.FaceURL, nil
		}
	}
	return "", nil
}

func asError(err error, target **Error) bool {
	e, ok := err.(*Error)
	if ok {
		*target = e
	}
	return ok
}

// Ping checks that OpenIM is reachable and the secret is accepted.
func (c *Client) Ping(ctx context.Context) error {
	_, err := c.AdminToken(ctx)
	return err
}

// SendText posts a text message as `sender` and returns its clientMsgID, the
// id every client addresses the message by. Exactly one of recvID (a direct
// chat) or groupID must be set.
//
// The admin token lets this service speak as any user, which is what makes a
// bot possible without the bot ever holding a connection.
func (c *Client) SendText(ctx context.Context, sender, nickname, recvID, groupID, text, ex string) (string, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return "", err
	}
	body := map[string]any{
		"sendID":           sender,
		"senderNickname":   nickname,
		"senderPlatformID": 7,
		"contentType":      101,
		"content":          map[string]string{"content": text},
	}
	if ex != "" {
		body["ex"] = ex
	}
	// 兜底的离线推送文案（做法 A）。正式的文案由 beforeOfflinePush 回调集中生成
	// （做法 B）；这份只在回调没开、或者回调挂了的时候用。声音必须自己带：适配器
	// 回补到 3.8.3 之后不再写死 default，不带就是无声通知。
	conversation := "sg_" + groupID
	if groupID == "" {
		conversation = push.DirectID(sender, recvID)
	}
	kind, runID := pushKind(ex)
	body["offlinePushInfo"] = map[string]any{
		"title": nickname, "desc": push.Preview(101, mustJSON(map[string]string{"content": text})),
		"ex": push.Ex(push.Route{Conversation: conversation, Kind: kind, RunID: runID}), "iOSPushSound": "default", "iOSBadgeCount": true,
	}
	if groupID != "" {
		body["groupID"] = groupID
		body["sessionType"] = 3
	} else {
		body["recvID"] = recvID
		body["sessionType"] = 1
	}
	var out struct {
		ClientMsgID string `json:"clientMsgID"`
	}
	if err := c.post(ctx, "/msg/send_msg", body, admin, &out); err != nil {
		return "", err
	}
	return out.ClientMsgID, nil
}

// SendCustom posts a custom message as `sender`: contentType 110 with yptd's
// own JSON inside. Reactions travel this way.
func (c *Client) SendCustom(ctx context.Context, sender, nickname, recvID, groupID, data, description string) (string, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return "", err
	}
	body := map[string]any{
		"sendID":           sender,
		"senderNickname":   nickname,
		"senderPlatformID": 7,
		"contentType":      110,
		"content":          map[string]string{"data": data, "description": description, "extension": ""},
		// yptd 的自定义消息是 reaction 和信号，不是给人看的，手机上不该响
		"notOfflinePush": true,
	}
	if groupID != "" {
		body["groupID"] = groupID
		body["sessionType"] = 3
	} else {
		body["recvID"] = recvID
		body["sessionType"] = 1
	}
	var out struct {
		ClientMsgID string `json:"clientMsgID"`
	}
	if err := c.post(ctx, "/msg/send_msg", body, admin, &out); err != nil {
		return "", err
	}
	return out.ClientMsgID, nil
}

// NewestSeq is the highest sequence number in a conversation, as seen by
// `userID`. Revoking needs a seq and sending does not return one, so this is
// how a just-sent message is found again.
func (c *Client) NewestSeq(ctx context.Context, userID, conversationID string) (int64, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return 0, err
	}
	var out struct {
		MaxSeqs map[string]int64 `json:"maxSeqs"`
	}
	body := map[string]any{"userID": userID, "conversationIDs": []string{conversationID}}
	if err := c.post(ctx, "/msg/newest_seq", body, admin, &out); err != nil {
		return 0, err
	}
	seq, ok := out.MaxSeqs[conversationID]
	if !ok {
		return 0, fmt.Errorf("openim /msg/newest_seq: no seq for %s", conversationID)
	}
	return seq, nil
}

// RevokeMsg withdraws one message. Used to take back a placeholder once the
// real answer is ready.
func (c *Client) RevokeMsg(ctx context.Context, userID, conversationID string, seq int64) error {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return err
	}
	body := map[string]any{"userID": userID, "conversationID": conversationID, "seq": seq}
	return c.post(ctx, "/msg/revoke_msg", body, admin, nil)
}

// JoinedGroups lists the group ids a user belongs to.
//
// The watch needs it to know where to post: an agent is reachable in exactly
// the rooms someone has invited it into, and that set changes without this
// service being told.
// Group is the part of an OpenIM group this service reads.
type Group struct {
	GroupID     string `json:"groupID"`
	GroupName   string `json:"groupName"`
	MemberCount int    `json:"memberCount"`
	Ex          string `json:"ex"`
	Status      int    `json:"status"`
	OwnerUserID string `json:"ownerUserID"`
}

// Dismissed reports whether this group is gone. OpenIM keeps dismissed groups
// in the table and search still returns them, so a directory that does not
// check this lists rooms nobody can enter.
func (g Group) Dismissed() bool { return g.Status == 2 }

// GroupInfo reads one group.
func (c *Client) GroupInfo(ctx context.Context, groupID string) (Group, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return Group{}, err
	}
	var out struct {
		GroupInfos []Group `json:"groupInfos"`
	}
	if err := c.post(ctx, "/group/get_groups_info", map[string]any{"groupIDs": []string{groupID}}, admin, &out); err != nil {
		return Group{}, err
	}
	if len(out.GroupInfos) == 0 {
		return Group{}, fmt.Errorf("openim: no such group %s", groupID)
	}
	return out.GroupInfos[0], nil
}

// SearchGroups finds groups by name.
//
// This is OpenIM's management route and needs the admin token, which is why a
// client cannot do it itself: the SDK's own searchGroups only reads the local
// database, so it can only ever find groups you have already joined. Finding
// a room you are not in has to come through here.
func (c *Client) SearchGroups(ctx context.Context, name string, limit int) ([]Group, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return nil, err
	}
	if limit <= 0 || limit > 100 {
		limit = 30
	}
	body := map[string]any{
		"groupName":  name,
		"pagination": map[string]int{"pageNumber": 1, "showNumber": limit},
	}
	// 注意外面还包了一层：groups[].groupInfo，不是 groups[] 直接就是群
	var out struct {
		Groups []struct {
			GroupInfo Group `json:"groupInfo"`
		} `json:"groups"`
	}
	if err := c.post(ctx, "/group/get_groups", body, admin, &out); err != nil {
		return nil, err
	}
	groups := make([]Group, 0, len(out.Groups))
	for _, g := range out.Groups {
		if g.GroupInfo.GroupID != "" {
			groups = append(groups, g.GroupInfo)
		}
	}
	return groups, nil
}

func (c *Client) JoinedGroups(ctx context.Context, userID string) ([]string, error) {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return nil, err
	}
	// One page of 500 is every group an agent will plausibly be in; a bot
	// that somehow passes it would only miss the tail, not break.
	body := map[string]any{
		"fromUserID": userID,
		"pagination": map[string]int{"pageNumber": 1, "showNumber": 500},
	}
	var out struct {
		Groups []struct {
			GroupID string `json:"groupID"`
		} `json:"groups"`
	}
	if err := c.post(ctx, "/group/get_joined_group_list", body, admin, &out); err != nil {
		return nil, err
	}
	ids := make([]string, 0, len(out.Groups))
	for _, g := range out.Groups {
		if g.GroupID != "" {
			ids = append(ids, g.GroupID)
		}
	}
	return ids, nil
}

func mustJSON(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}

// pushKind 从消息的 ex 里认出「这是一次 run 的回答」。推送规则靠它把 agent 的回答
// 和行情、新闻这类主动播报分开：回答推，播报不推。ex 的形状见 bot.runEx。
func pushKind(ex string) (kind, runID string) {
	if ex == "" {
		return "", ""
	}
	var p struct {
		Yptd string `json:"yptd"`
		Run  string `json:"run"`
	}
	if json.Unmarshal([]byte(ex), &p) != nil || p.Yptd != "run" {
		return "", ""
	}
	return push.Answer, p.Run
}
