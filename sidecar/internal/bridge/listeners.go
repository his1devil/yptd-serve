// Package bridge adapts openim-sdk-core's callback interfaces to the wire
// protocol.
//
// The SDK speaks in Go interfaces with one method per event and JSON strings
// for payloads. Everything here is mechanical translation: the event name is
// the method name, and the payload passes through untouched so the client sees
// exactly what the SDK produced rather than something re-encoded on the way.
package bridge

import (
	"encoding/json"

	"github.com/his1devil/yptd/sidecar/internal/protocol"
)

// Emitter is the half of the writer the listeners need.
type Emitter interface {
	Emit(name string, data json.RawMessage) error
	EmitValue(name string, value any) error
}

// raw forwards a JSON string the SDK already produced. When it is not valid
// JSON -- some callbacks hand over a bare string -- it is wrapped so the frame
// stays parseable.
func raw(e Emitter, name, payload string) {
	if json.Valid([]byte(payload)) && payload != "" {
		_ = e.Emit(name, json.RawMessage(payload))
		return
	}
	_ = e.EmitValue(name, map[string]string{"value": payload})
}

// Conn reports connection lifecycle.
type Conn struct{ Out Emitter }

func (c Conn) OnConnecting()       { _ = c.Out.Emit("OnConnecting", nil) }
func (c Conn) OnConnectSuccess()   { _ = c.Out.Emit("OnConnectSuccess", nil) }
func (c Conn) OnKickedOffline()    { _ = c.Out.Emit("OnKickedOffline", nil) }
func (c Conn) OnUserTokenExpired() { _ = c.Out.Emit("OnUserTokenExpired", nil) }

func (c Conn) OnConnectFailed(code int32, msg string) {
	_ = c.Out.EmitValue("OnConnectFailed", map[string]any{"code": code, "msg": msg})
}

func (c Conn) OnUserTokenInvalid(msg string) {
	_ = c.Out.EmitValue("OnUserTokenInvalid", map[string]string{"msg": msg})
}

// Conversation reports the conversation list and the initial sync.
type Conversation struct{ Out Emitter }

func (c Conversation) OnSyncServerStart(reinstalled bool) {
	_ = c.Out.EmitValue("OnSyncServerStart", map[string]bool{"reinstalled": reinstalled})
}
func (c Conversation) OnSyncServerFinish(reinstalled bool) {
	_ = c.Out.EmitValue("OnSyncServerFinish", map[string]bool{"reinstalled": reinstalled})
}
func (c Conversation) OnSyncServerFailed(reinstalled bool) {
	_ = c.Out.EmitValue("OnSyncServerFailed", map[string]bool{"reinstalled": reinstalled})
}
func (c Conversation) OnSyncServerProgress(progress int) {
	_ = c.Out.EmitValue("OnSyncServerProgress", map[string]int{"progress": progress})
}
func (c Conversation) OnNewConversation(list string) { raw(c.Out, "OnNewConversation", list) }
func (c Conversation) OnConversationChanged(list string) {
	raw(c.Out, "OnConversationChanged", list)
}
func (c Conversation) OnTotalUnreadMessageCountChanged(total int32) {
	_ = c.Out.EmitValue("OnTotalUnreadMessageCountChanged", map[string]int32{"total": total})
}
func (c Conversation) OnConversationUserInputStatusChanged(change string) {
	raw(c.Out, "OnConversationUserInputStatusChanged", change)
}

// Message reports incoming messages and their edits.
type Message struct{ Out Emitter }

func (m Message) OnRecvNewMessage(msg string)          { raw(m.Out, "OnRecvNewMessage", msg) }
func (m Message) OnRecvOfflineNewMessage(msg string)   { raw(m.Out, "OnRecvOfflineNewMessage", msg) }
func (m Message) OnRecvOnlineOnlyMessage(msg string)   { raw(m.Out, "OnRecvOnlineOnlyMessage", msg) }
func (m Message) OnMsgDeleted(msg string)              { raw(m.Out, "OnMsgDeleted", msg) }
func (m Message) OnNewRecvMessageRevoked(msg string)   { raw(m.Out, "OnNewRecvMessageRevoked", msg) }
func (m Message) OnRecvC2CReadReceipt(receipts string) { raw(m.Out, "OnRecvC2CReadReceipt", receipts) }

// Batch reports offline backlogs, which arrive as arrays rather than singly.
type Batch struct{ Out Emitter }

func (b Batch) OnRecvNewMessages(list string)        { raw(b.Out, "OnRecvNewMessages", list) }
func (b Batch) OnRecvOfflineNewMessages(list string) { raw(b.Out, "OnRecvOfflineNewMessages", list) }

// Group reports membership and group metadata.
type Group struct{ Out Emitter }

func (g Group) OnJoinedGroupAdded(info string)       { raw(g.Out, "OnJoinedGroupAdded", info) }
func (g Group) OnJoinedGroupDeleted(info string)     { raw(g.Out, "OnJoinedGroupDeleted", info) }
func (g Group) OnGroupMemberAdded(info string)       { raw(g.Out, "OnGroupMemberAdded", info) }
func (g Group) OnGroupMemberDeleted(info string)     { raw(g.Out, "OnGroupMemberDeleted", info) }
func (g Group) OnGroupInfoChanged(info string)       { raw(g.Out, "OnGroupInfoChanged", info) }
func (g Group) OnGroupDismissed(info string)         { raw(g.Out, "OnGroupDismissed", info) }
func (g Group) OnGroupMemberInfoChanged(info string) { raw(g.Out, "OnGroupMemberInfoChanged", info) }
func (g Group) OnGroupApplicationAdded(a string)     { raw(g.Out, "OnGroupApplicationAdded", a) }
func (g Group) OnGroupApplicationDeleted(a string)   { raw(g.Out, "OnGroupApplicationDeleted", a) }
func (g Group) OnGroupApplicationAccepted(a string)  { raw(g.Out, "OnGroupApplicationAccepted", a) }
func (g Group) OnGroupApplicationRejected(a string)  { raw(g.Out, "OnGroupApplicationRejected", a) }

// User reports profile and presence.
type User struct{ Out Emitter }

func (u User) OnSelfInfoUpdated(info string)     { raw(u.Out, "OnSelfInfoUpdated", info) }
func (u User) OnUserStatusChanged(status string) { raw(u.Out, "OnUserStatusChanged", status) }
func (u User) OnUserCommandAdd(c string)         { raw(u.Out, "OnUserCommandAdd", c) }
func (u User) OnUserCommandDelete(c string)      { raw(u.Out, "OnUserCommandDelete", c) }
func (u User) OnUserCommandUpdate(c string)      { raw(u.Out, "OnUserCommandUpdate", c) }

// compile-time assertion that Emitter matches what protocol.Writer offers.
var _ Emitter = (*protocol.Writer)(nil)
