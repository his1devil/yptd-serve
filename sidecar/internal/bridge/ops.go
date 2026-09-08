package bridge

import (
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/openimsdk/openim-sdk-core/v3/open_im_sdk"
)

// callTimeout bounds any single SDK call. The SDK retries internally and can
// otherwise wait on a dead connection indefinitely; a request that never
// answers is worse for the client than one that fails.
const callTimeout = 45 * time.Second

// promise adapts the SDK's callback interface to a single awaited result.
//
// Every SDK entry point takes a callback and returns immediately. Bridging
// that to a request/response protocol means turning "call me back" into "wait
// here", which is what this does -- with a buffered channel, so a callback
// that fires after the caller has given up does not leak a goroutine.
type promise struct {
	done chan result
	once sync.Once
}

type result struct {
	data string
	code int32
	msg  string
}

func newPromise() *promise {
	return &promise{done: make(chan result, 1)}
}

func (p *promise) OnSuccess(data string) {
	p.once.Do(func() { p.done <- result{data: data} })
}

func (p *promise) OnError(code int32, msg string) {
	p.once.Do(func() { p.done <- result{code: code, msg: msg} })
}

// OnProgress satisfies SendMsgCallBack. Upload progress is not surfaced yet;
// swallowing it here keeps the send path on the same promise as everything
// else rather than needing a second mechanism.
func (p *promise) OnProgress(int) {}

var errTimeout = errors.New("SDK 调用超时")

func (p *promise) wait() (string, error) {
	select {
	case r := <-p.done:
		if r.msg != "" || r.code != 0 {
			return "", &SDKError{Code: r.code, Msg: r.msg}
		}
		return r.data, nil
	case <-time.After(callTimeout):
		return "", errTimeout
	}
}

// SDKError carries the SDK's own error code so the client can distinguish
// "token expired" from "network down" without string matching.
type SDKError struct {
	Code int32
	Msg  string
}

func (e *SDKError) Error() string { return fmt.Sprintf("%d %s", e.Code, e.Msg) }

var opSeq atomic.Uint64

func operationID() string {
	return "yptd-" + strconv.FormatUint(opSeq.Add(1), 36) + "-" +
		strconv.FormatInt(time.Now().UnixMilli(), 36)
}

// Handler dispatches one operation name to the SDK.
type Handler struct {
	Out Emitter
}

// args decodes a request's arguments into `into`, defaulting to an empty
// object so an op with all-optional fields can be called with no args at all.
func args(raw json.RawMessage, into any) error {
	if len(raw) == 0 {
		raw = json.RawMessage("{}")
	}
	if err := json.Unmarshal(raw, into); err != nil {
		return fmt.Errorf("参数解析失败: %w", err)
	}
	return nil
}

// jsonOrString wraps an SDK reply for transport. Most calls return JSON; a few
// return a bare string (a conversation id, a created message). Wrapping the
// latter keeps every reply a valid JSON value.
func jsonOrString(data string) json.RawMessage {
	if data == "" {
		return json.RawMessage("null")
	}
	if json.Valid([]byte(data)) {
		return json.RawMessage(data)
	}
	quoted, _ := json.Marshal(data)
	return quoted
}

// Dispatch runs one operation and returns its reply payload.
func (h *Handler) Dispatch(op string, raw json.RawMessage) (json.RawMessage, error) {
	switch op {

	// ---------------------------------------------------------- lifecycle ---
	case "init":
		var a struct {
			APIAddr    string `json:"api_addr"`
			WSAddr     string `json:"ws_addr"`
			DataDir    string `json:"data_dir"`
			PlatformID int32  `json:"platform_id"`
			LogLevel   uint32 `json:"log_level"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		if a.PlatformID == 0 {
			a.PlatformID = 7 // Linux
		}
		if a.LogLevel == 0 {
			a.LogLevel = 3
		}
		config, _ := json.Marshal(map[string]any{
			"apiAddr":              a.APIAddr,
			"wsAddr":               a.WSAddr,
			"dataDir":              a.DataDir,
			"platformID":           a.PlatformID,
			"logLevel":             a.LogLevel,
			"isLogStandardOutput":  false,
			"logFilePath":          a.DataDir,
			"isExternalExtensions": false,
		})
		// InitSDK creates the SDK's user object; every Set*Listener before it
		// is dropped with a "UserForSDK is nil" warning. So: init, then
		// listeners, then login -- login is what starts the traffic.
		if !open_im_sdk.InitSDK(Conn{Out: h.Out}, operationID(), string(config)) {
			return nil, errors.New("InitSDK 失败，检查 api_addr / ws_addr 格式")
		}
		open_im_sdk.SetConversationListener(Conversation{Out: h.Out})
		open_im_sdk.SetAdvancedMsgListener(Message{Out: h.Out})
		open_im_sdk.SetBatchMsgListener(Batch{Out: h.Out})
		open_im_sdk.SetGroupListener(Group{Out: h.Out})
		open_im_sdk.SetUserListener(User{Out: h.Out})
		return json.RawMessage(`{"ok":true}`), nil

	case "login":
		var a struct {
			UserID string `json:"user_id"`
			Token  string `json:"token"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		p := newPromise()
		open_im_sdk.Login(p, operationID(), a.UserID, a.Token)
		return h.reply(p)

	case "logout":
		p := newPromise()
		open_im_sdk.Logout(p, operationID())
		return h.reply(p)

	case "login_status":
		return json.RawMessage(strconv.Itoa(open_im_sdk.GetLoginStatus(operationID()))), nil

	case "sdk_version":
		return jsonOrString(open_im_sdk.GetSdkVersion()), nil

	// ------------------------------------------------------ conversations ---
	case "conversations":
		p := newPromise()
		open_im_sdk.GetAllConversationList(p, operationID())
		return h.reply(p)

	case "conversation_id":
		var a struct {
			SourceID    string `json:"source_id"`
			SessionType int    `json:"session_type"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		return jsonOrString(open_im_sdk.GetConversationIDBySessionType(
			operationID(), a.SourceID, a.SessionType)), nil

	case "mark_read":
		var a struct {
			ConversationID string `json:"conversation_id"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		p := newPromise()
		open_im_sdk.MarkConversationMessageAsRead(p, operationID(), a.ConversationID)
		return h.reply(p)

	// ----------------------------------------------------------- messages ---
	case "history":
		// Passed through verbatim: the SDK's option struct has a dozen fields
		// and mirroring it here would mean a second place to keep in step.
		if len(raw) == 0 {
			return nil, errors.New("history 需要参数")
		}
		p := newPromise()
		open_im_sdk.GetAdvancedHistoryMessageList(p, operationID(), string(raw))
		return h.reply(p)

	case "create_text":
		var a struct {
			Text string `json:"text"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		return jsonOrString(open_im_sdk.CreateTextMessage(operationID(), a.Text)), nil

	case "create_quote":
		var a struct {
			Text  string          `json:"text"`
			Quote json.RawMessage `json:"quote"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		return jsonOrString(open_im_sdk.CreateQuoteMessage(
			operationID(), a.Text, string(a.Quote))), nil

	// One primitive covers @ alone and @ with a quote: the SDK nests the
	// quoted message inside the at-text element rather than making them two
	// different content types.
	case "create_at_text":
		var a struct {
			Text    string          `json:"text"`
			UserIDs []string        `json:"at_user_ids"`
			AtInfo  json.RawMessage `json:"at_users_info"`
			Quote   json.RawMessage `json:"quote"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		if len(a.UserIDs) == 0 {
			return nil, errors.New("create_at_text 需要至少一个 at_user_ids")
		}
		ids, _ := json.Marshal(a.UserIDs)
		info := "[]"
		if len(a.AtInfo) > 0 {
			info = string(a.AtInfo)
		}
		// An empty string becomes a nil *MsgStruct on the SDK side, which is
		// what "no quote" means there. A literal JSON null means the same.
		quote := ""
		if len(a.Quote) > 0 && string(a.Quote) != "null" {
			quote = string(a.Quote)
		}
		return jsonOrString(open_im_sdk.CreateTextAtMessage(
			operationID(), a.Text, string(ids), info, quote)), nil

	// The quote primitives take the whole original message, not an id, so a
	// reply first has to fetch what it is replying to out of the local store.
	case "find_message":
		var a struct {
			ConversationID string   `json:"conversation_id"`
			ClientMsgIDs   []string `json:"client_msg_ids"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		query, _ := json.Marshal([]map[string]any{{
			"conversationID":  a.ConversationID,
			"clientMsgIDList": a.ClientMsgIDs,
		}})
		p := newPromise()
		open_im_sdk.FindMessageList(p, operationID(), string(query))
		return h.reply(p)

	// "@everyone" is a reserved user id rather than a flag; ask the SDK for
	// it instead of hardcoding a string that could change under us.
	case "at_all_tag":
		return jsonOrString(open_im_sdk.GetAtAllTag(operationID())), nil

	case "create_image":
		var a struct {
			Path string `json:"path"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		return jsonOrString(open_im_sdk.CreateImageMessageFromFullPath(operationID(), a.Path)), nil

	case "send":
		var a struct {
			Message      json.RawMessage `json:"message"`
			RecvID       string          `json:"recv_id"`
			GroupID      string          `json:"group_id"`
			OfflinePush  json.RawMessage `json:"offline_push"`
			IsOnlineOnly bool            `json:"online_only"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		push := "{}"
		if len(a.OfflinePush) > 0 {
			push = string(a.OfflinePush)
		}
		p := newPromise()
		open_im_sdk.SendMessage(p, operationID(), string(a.Message),
			a.RecvID, a.GroupID, push, a.IsOnlineOnly)
		return h.reply(p)

	case "revoke":
		var a struct {
			ConversationID string `json:"conversation_id"`
			ClientMsgID    string `json:"client_msg_id"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		p := newPromise()
		open_im_sdk.RevokeMessage(p, operationID(), a.ConversationID, a.ClientMsgID)
		return h.reply(p)

	case "typing":
		var a struct {
			ConversationID string `json:"conversation_id"`
			Typing         bool   `json:"typing"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		p := newPromise()
		open_im_sdk.ChangeInputStates(p, operationID(), a.ConversationID, a.Typing)
		return h.reply(p)

	// ------------------------------------------------------------- groups ---
	case "groups":
		p := newPromise()
		open_im_sdk.GetJoinedGroupList(p, operationID())
		return h.reply(p)

	case "group_members":
		var a struct {
			GroupID string `json:"group_id"`
			Filter  int32  `json:"filter"`
			Offset  int32  `json:"offset"`
			Count   int32  `json:"count"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		if a.Count == 0 {
			a.Count = 200
		}
		p := newPromise()
		open_im_sdk.GetGroupMemberList(p, operationID(), a.GroupID, a.Filter, a.Offset, a.Count)
		return h.reply(p)

	case "create_group":
		if len(raw) == 0 {
			return nil, errors.New("create_group 需要参数")
		}
		p := newPromise()
		open_im_sdk.CreateGroup(p, operationID(), string(raw))
		return h.reply(p)

	case "invite":
		var a struct {
			GroupID string   `json:"group_id"`
			Reason  string   `json:"reason"`
			UserIDs []string `json:"user_ids"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		ids, _ := json.Marshal(a.UserIDs)
		p := newPromise()
		open_im_sdk.InviteUserToGroup(p, operationID(), a.GroupID, a.Reason, string(ids))
		return h.reply(p)

	// -------------------------------------------------------------- users ---
	case "self":
		p := newPromise()
		open_im_sdk.GetSelfUserInfo(p, operationID())
		return h.reply(p)

	case "users":
		var a struct {
			UserIDs []string `json:"user_ids"`
		}
		if err := args(raw, &a); err != nil {
			return nil, err
		}
		ids, _ := json.Marshal(a.UserIDs)
		p := newPromise()
		open_im_sdk.GetUsersInfo(p, operationID(), string(ids))
		return h.reply(p)

	default:
		return nil, fmt.Errorf("未知操作 %q", op)
	}
}

func (h *Handler) reply(p *promise) (json.RawMessage, error) {
	data, err := p.wait()
	if err != nil {
		return nil, err
	}
	return jsonOrString(data), nil
}
