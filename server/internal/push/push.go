// Package push decides what an offline push notification says and who gets
// it. It answers OpenIM's beforeOfflinePush webhook.
//
// 为什么放服务端而不是让每个客户端自己填：通知长什么样、哪些消息该推，是三端
// （iOS、桌面、Go 服务）共用的规则。让每端自己填就是三份实现外加一批管不住的
// 旧版本；回调集中定，客户端填的那份只当兜底。
//
// 这个包不碰网络，Decide 是纯函数：名字查询通过 Names 注入，好测。
package push

import (
	"encoding/json"
	"sort"
	"strings"
	"unicode/utf8"
)

// OpenIM 的消息类型，只列这里用到的。
const (
	Text     = 101
	Picture  = 102
	Voice    = 103
	Video    = 104
	File     = 105
	AtText   = 106
	Custom   = 110
	Typing   = 113
	Quote    = 114
	Notifies = 1000 // 系统通知从这里起
)

// AtAll 是 OpenIM 里「@所有人」在 atUserIDList 里的写法。
const AtAll = "AtAllTag"

// Request 是 OpenIM 在推离线通知之前问过来的内容。字段名照 OpenIM 的 JSON。
type Request struct {
	UserIDs       []string `json:"userIDList"`
	Title         string   `json:"title"`
	Desc          string   `json:"desc"`
	Ex            string   `json:"ex"`
	IOSPushSound  string   `json:"iOSPushSound"`
	IOSBadgeCount bool     `json:"iOSBadgeCount"`
	ClientMsgID   string   `json:"clientMsgID"`
	SendID        string   `json:"sendID"`
	GroupID       string   `json:"groupID"`
	ContentType   int32    `json:"contentType"`
	SessionType   int32    `json:"sessionType"`
	AtUserIDs     []string `json:"atUserIDList"`
	Content       string   `json:"content"`
}

// Info is what the phone shows. Mirrors OpenIM's OfflinePushInfo.
type Info struct {
	Title         string `json:"title"`
	Desc          string `json:"desc"`
	Ex            string `json:"ex"`
	IOSPushSound  string `json:"iOSPushSound"`
	IOSBadgeCount bool   `json:"iOSBadgeCount"`
}

// Decision 是回答。Skip 为真时这条不推，Reason 说为什么（进日志，不给 OpenIM）。
//
// UserIDs 为 nil 表示「照 OpenIM 原来的名单」。**只在群消息里才会非 nil**：OpenIM
// 单聊那条路径把接收方指针传的是 nil，回一份名单回去它会空指针崩掉 openim-push。
type Decision struct {
	Skip    bool
	Reason  string
	UserIDs []string
	Info    Info
}

// Names 是决定文案要查的两个名字，加上「这是不是 agent」。
type Names interface {
	Nickname(userID string) string
	GroupName(groupID string) string
	IsAgent(userID string) bool
}

// Decide 按规则给出答案。
//
// 规则（2026-09-18 定）：
//   - 私聊：推。
//   - 群里：只推被 @ 到的人；@所有人 就推全部。没 @ 任何人的普通消息不推——
//     群里聊天每条都响手机，第一天就会被关掉通知。
//   - 不推：reaction 之类的自定义消息、输入状态、系统通知、agent 的「正在处理」占位。
//   - agent 账号没有手机，从名单里去掉。
//   - 文案：私聊「发送者 / 预览」，群里「#群名 / 发送者：预览」。
func Decide(req Request, names Names, placeholder string) Decision {
	switch {
	case req.ContentType == Custom:
		return skip("custom message (reaction or signal)")
	case req.ContentType == Typing:
		return skip("typing")
	case req.ContentType >= Notifies:
		return skip("system notification")
	}
	preview := Preview(req.ContentType, req.Content)
	if placeholder != "" && names.IsAgent(req.SendID) && strings.TrimSpace(preview) == strings.TrimSpace(placeholder) {
		return skip("agent placeholder")
	}

	group := req.SessionType == 3 || req.GroupID != ""
	recipients := req.UserIDs
	if group {
		recipients = mentioned(req.UserIDs, mentions(req))
	}
	recipients = without(recipients, req.SendID, names.IsAgent)
	if len(recipients) == 0 {
		if group {
			return skip("group message with nobody mentioned")
		}
		return skip("no recipient left")
	}

	sender := names.Nickname(req.SendID)
	if sender == "" {
		sender = req.SendID
	}
	var conversation string
	info := Info{IOSPushSound: "default", IOSBadgeCount: true}
	if group {
		conversation = "sg_" + req.GroupID
		name := names.GroupName(req.GroupID)
		if name == "" {
			name = "群聊"
		}
		info.Title = "#" + name
		info.Desc = sender + "：" + preview
	} else {
		conversation = DirectID(req.SendID, recipients[0])
		info.Title = sender
		info.Desc = preview
	}
	kind, runID := kindOf(req.Ex)
	info.Ex = Ex(conversation, kind, runID)

	d := Decision{Info: info}
	if group && !sameSet(recipients, req.UserIDs) {
		d.UserIDs = recipients
	}
	return d
}

func skip(reason string) Decision { return Decision{Skip: true, Reason: reason} }

// Ex 是通知里带给手机的那段 JSON，手机按 conversation_id 跳会话。三端约定见
// docs/client-api.md。kind 留空当 message。
func Ex(conversationID, kind, runID string) string {
	if kind == "" {
		kind = "message"
	}
	m := map[string]any{"v": 1, "conversation_id": conversationID, "kind": kind}
	if runID != "" {
		m["run_id"] = runID
	}
	b, _ := json.Marshal(m)
	return string(b)
}

// kindOf 读发送方兜底填的 ex：它可能已经说了这是 run_result 并带了 run_id，这两个
// 字段回调补不出来，得留住。conversation_id 不信它的，重新算。
func kindOf(ex string) (kind, runID string) {
	if ex == "" {
		return "", ""
	}
	var p struct {
		V     int    `json:"v"`
		Kind  string `json:"kind"`
		RunID string `json:"run_id"`
	}
	if json.Unmarshal([]byte(ex), &p) != nil || p.V != 1 {
		return "", ""
	}
	return p.Kind, p.RunID
}

// DirectID 是私聊会话 id：两个 user id 按字典序拼，和客户端算法一致。
func DirectID(a, b string) string {
	if b < a {
		a, b = b, a
	}
	return "si_" + a + "_" + b
}

// Preview 把消息内容变成一行能放进通知的文字。
func Preview(contentType int32, content string) string {
	var text string
	switch contentType {
	case Text:
		var e struct {
			Content string `json:"content"`
		}
		_ = json.Unmarshal([]byte(content), &e)
		text = e.Content
	case AtText, Quote:
		var e struct {
			Text string `json:"text"`
		}
		_ = json.Unmarshal([]byte(content), &e)
		text = e.Text
	case Picture:
		text = "[图片]"
	case Voice:
		text = "[语音]"
	case Video:
		text = "[视频]"
	case File:
		var e struct {
			FileName string `json:"fileName"`
		}
		_ = json.Unmarshal([]byte(content), &e)
		text = strings.TrimSpace("[文件] " + e.FileName)
	default:
		text = "[消息]"
	}
	text = strings.Join(strings.Fields(text), " ")
	if text == "" {
		text = "[消息]"
	}
	return cut(text, 80)
}

func cut(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	r := []rune(s)
	return string(r[:n-1]) + "…"
}

// mentions 合并两处的 @ 名单：消息头上的 atUserIDList 是 SDK 发消息时填的，管理接口
// （REST /msg/send_msg，Go 服务和脚本走这条）只填元素里面那份。只看一处就会漏掉一半
// 的来路——测试时用管理接口 @ 人，回调里 atUserIDList 是空的，整条被判成「没 @ 任何人」。
func mentions(req Request) []string {
	out := append([]string(nil), req.AtUserIDs...)
	if req.ContentType == AtText {
		var e struct {
			AtUserList []string `json:"atUserList"`
		}
		if json.Unmarshal([]byte(req.Content), &e) == nil {
			out = append(out, e.AtUserList...)
		}
	}
	return out
}

// mentioned 是名单里被 @ 到的那部分；@所有人 就是整个名单。
func mentioned(userIDs, at []string) []string {
	set := make(map[string]bool, len(at))
	for _, id := range at {
		if id == AtAll {
			return userIDs
		}
		set[id] = true
	}
	var out []string
	for _, id := range userIDs {
		if set[id] {
			out = append(out, id)
		}
	}
	return out
}

// without 去掉发送者本人和 agent 账号，顺序不变。
func without(ids []string, sender string, isAgent func(string) bool) []string {
	var out []string
	for _, id := range ids {
		if id == "" || id == sender || isAgent(id) {
			continue
		}
		out = append(out, id)
	}
	return out
}

func sameSet(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	x, y := append([]string(nil), a...), append([]string(nil), b...)
	sort.Strings(x)
	sort.Strings(y)
	for i := range x {
		if x[i] != y[i] {
			return false
		}
	}
	return true
}
