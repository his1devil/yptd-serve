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
)

type Client struct {
	base    string
	secret  string
	adminID string
	http    *http.Client

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
func (c *Client) UpdateUser(ctx context.Context, userID, nickname string) error {
	token, err := c.AdminToken(ctx)
	if err != nil {
		return err
	}
	// OpenIM wants the fields nested; a flat body comes back as "UserInfo is empty".
	body := map[string]any{"userInfo": map[string]any{"userID": userID, "nickname": nickname}}
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

// SendText posts a text message as `sender`. Exactly one of recvID (a direct
// chat) or groupID must be set.
//
// The admin token lets this service speak as any user, which is what makes a
// bot possible without the bot ever holding a connection.
func (c *Client) SendText(ctx context.Context, sender, nickname, recvID, groupID, text, ex string) error {
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return err
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
	if groupID != "" {
		body["groupID"] = groupID
		body["sessionType"] = 3
	} else {
		body["recvID"] = recvID
		body["sessionType"] = 1
	}
	return c.post(ctx, "/msg/send_msg", body, admin, nil)
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
