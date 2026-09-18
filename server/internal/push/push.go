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
	"regexp"
	"sort"
	"strings"
	"unicode"
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

// Answer 是 ex 里标记「这条是 agent 对某人提问的回答」的 kind。agent 在群里说话
// 分两种：回答（有人问了才说）和主动播报（行情、新闻、配置变更，定时发的）。前者
// 该响手机，后者不该——每天几十条行情把通知变成噪音，人就把整个 app 静音了。
// 发送方在 offlinePushInfo.ex 里标这个 kind，见 openim.Client.SendText。
const Answer = "run_result"

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
	RecvID        string   `json:"recvID"`
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
	kind, runID := kindOf(req.Ex)

	group := req.SessionType == 3 || req.GroupID != ""
	if group && names.IsAgent(req.SendID) && kind != Answer {
		// agent 主动播报。回答有人的提问带 Answer 标记，照推。
		return skip("agent broadcast")
	}
	recipients := without(req.UserIDs, req.SendID, names.IsAgent)
	if len(recipients) == 0 {
		return skip("no recipient left")
	}
	var at []string
	if group {
		at = mentions(req, recipients)
	}

	sender := name(names.Nickname(req.SendID))
	if sender == "" {
		sender = name(req.SendID)
	}
	var conversation string
	info := Info{IOSPushSound: "default", IOSBadgeCount: true}
	if group {
		conversation = "sg_" + req.GroupID
		room := name(names.GroupName(req.GroupID))
		if room == "" {
			room = "群聊"
		}
		info.Title = "#" + room
		info.Desc = sender + "：" + preview
	} else {
		// 用 recvID 而不是 recipients[0]：名单只有一个人是 OpenIM 当前实现的巧合
		// （单聊路径写死 [msg.RecvID]），不是契约。猜错了就是点通知跳到别人的会话，
		// 而且不会报错。recvID 空了才退回名单。
		peer := req.RecvID
		if peer == "" {
			peer = recipients[0]
		}
		conversation = DirectID(req.SendID, peer)
		info.Title = sender
		info.Desc = preview
	}
	info.Ex = Ex(Route{Conversation: conversation, Kind: kind, RunID: runID, MsgID: req.ClientMsgID, At: at})

	d := Decision{Info: info}
	if group && !sameSet(recipients, req.UserIDs) {
		d.UserIDs = recipients
	}
	return d
}

func skip(reason string) Decision { return Decision{Skip: true, Reason: reason} }

// Route 是通知里带给手机的那段 JSON。三端约定见 docs/client-api.md。
type Route struct {
	// Conversation 是手机要跳到哪个会话，`sg_<群>` 或 `si_<a>_<b>`。必填。
	Conversation string
	// Kind 留空当 message；Answer 表示这是 agent 对某人提问的回答。
	Kind string
	// RunID 只有 Answer 才有，手机可以跳回那次 run。
	RunID string
	// MsgID 是这条消息的 clientMsgID。手机靠它精确等待并高亮那一条——从通知进会话时
	// 本地往往还没同步到它，没有这个 id 就只能等一个粗粒度的「同步完了」信号。
	// （OpenIM 自己的极光适配器也会塞 ClientMsgID，但那个字段全链路没人赋值，是死的。）
	MsgID string
	// At 是这条消息 @ 到的人，`["*"]` 表示 @所有人。一条群消息只有一份通知文案，服务端
	// 没法对被 @ 的人说「提到了你」、对其他人说别的；把名单放进来，手机端知道自己是谁，
	// 展示时自己加重。
	At []string
}

// Ex 把 Route 编成通知里那段 JSON。
func Ex(r Route) string {
	kind := r.Kind
	if kind == "" {
		kind = "message"
	}
	m := map[string]any{"v": 1, "conversation_id": r.Conversation, "kind": kind}
	if r.RunID != "" {
		m["run_id"] = r.RunID
	}
	if r.MsgID != "" {
		m["msg_id"] = r.MsgID
	}
	if len(r.At) > 0 {
		m["at"] = r.At
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
	text = strings.Join(strings.Fields(plain(text)), " ")
	if text == "" {
		text = "[消息]"
	}
	return cut(text, 80)
}

// name 把一个昵称或群名弄成能拼进通知标题的样子。
//
// 昵称是用户自己填的，中间的换行和制表符不会被注册时的 TrimSpace 拦住；群名更是直接
// 从 OpenIM 读出来的，服务端没有任何写入校验。一个带换行的昵称会把通知标题断成两行，
// 真正的内容被挤到看不见的地方。上限取 16 个字符：再长标题也显示不下，还会把正文顶掉。
func name(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	return cut(s, 16)
}

var (
	// 围栏代码块：整段换掉，不然 ``` 和缩进会把预览吃光
	fence = regexp.MustCompile("(?s)```.*?```|~~~.*?~~~")
	// 表格分隔行 |---|:---:| 和水平分割线 --- *** ___，整行没有信息。
	// Go 的正则是 RE2，没有反向引用，所以三种分割线只能分开写。
	ruleLine = regexp.MustCompile(`(?m)^[ \t]*\|?[ \t:|-]*\|[ \t:|-]*$|^[ \t]*(-{3,}|\*{3,}|_{3,})[ \t]*$`)
	// 行首标记：标题 #、引用 >、列表 - * +
	leadMark = regexp.MustCompile(`(?m)^[ \t]*(#{1,6}[ \t]+|>[ \t]*|[-*+][ \t]+)`)
	// 图片和链接：![alt](url) 留 [图片]，[text](url) 留 text
	image = regexp.MustCompile(`!\[[^\]]*\]\([^)]*\)`)
	link  = regexp.MustCompile(`\[([^\]]*)\]\([^)]*\)`)
	// 行内强调：**x** *x* __x__ ~~x~~ `x`。单下划线不碰，user_id 这类标识符会被误伤。
	emphasis = regexp.MustCompile("\\*{1,3}([^*\\n]+)\\*{1,3}|__([^_\\n]+)__|~~([^~\\n]+)~~|`([^`\\n]+)`")
)

// plain 把 markdown 压成一行能读的纯文本。
//
// 这不是 markdown 解析器，也不需要是——它只服务于 80 个字符的通知预览。agent 的回答
// 几乎都是 markdown（Charlie 的格式就是「结论 + 表格 + 风险」），不剥的话锁屏上是
// 「## 结论 | 标的 | 涨跌 | |---|---| | NVDA.US | +3.2%」，分隔行白占十几个字符，
// 用户一个有效字都看不到。
func plain(s string) string {
	// 快速路径要包含每一种标记的首字符，漏一个就是那种 markdown 不剥——列表的
	// `-` `+` 漏过一次。`-` 在行情文本里很常见，那种就老老实实走下面。
	if !strings.ContainsAny(s, "#*_`|>[~-+") {
		return s
	}
	s = fence.ReplaceAllString(s, " [代码] ")
	s = ruleLine.ReplaceAllString(s, " ")
	s = leadMark.ReplaceAllString(s, "")
	s = image.ReplaceAllString(s, " [图片] ")
	s = link.ReplaceAllString(s, "$1")
	s = emphasis.ReplaceAllString(s, "$1$2$3$4")
	// 表格的竖线换成空格，让单元格之间还有个间隔
	s = strings.ReplaceAll(s, "|", " ")
	return s
}

// cut 截到 n 个字符，末尾加省略号。
//
// 不切在 emoji 组合序列中间：一个看得见的 emoji 常常不止一个 rune——ZWJ 家庭是 7 个、
// 国旗是 2 个区域指示符、带肤色的手势是 2 个。切在中间会在结尾留下孤零零的色块、字母
// 方框或者豆腐块。没引 grapheme 库，退而求其次：把结尾的续接字符剥掉。
func cut(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	r := []rune(s)[:n-1]
	for len(r) > 0 {
		last := r[len(r)-1]
		switch {
		case last == 0x200D, // 零宽连接符
			last >= 0xFE00 && last <= 0xFE0F,                           // 变体选择符
			last >= 0x1F3FB && last <= 0x1F3FF,                         // 肤色
			unicode.Is(unicode.Mn, last), unicode.Is(unicode.Me, last): // 组合记号
			r = r[:len(r)-1]
			continue
		case last >= 0x1F1E6 && last <= 0x1F1FF: // 区域指示符：国旗是两个，落单的要去掉
			run := 0
			for i := len(r) - 1; i >= 0 && r[i] >= 0x1F1E6 && r[i] <= 0x1F1FF; i-- {
				run++
			}
			if run%2 == 1 {
				r = r[:len(r)-1]
				continue
			}
		}
		break
	}
	return string(r) + "…"
}

// mentions 合并两处的 @ 名单：消息头上的 atUserIDList 是 SDK 发消息时填的，管理接口
// （REST /msg/send_msg，Go 服务和脚本走这条）只填元素里面那份。只看一处就会漏掉一半
// 的来路——测试时用管理接口 @ 人，回调里 atUserIDList 是空的，整条被判成「没 @ 任何人」。
func mentions(req Request, recipients []string) []string {
	raw := append([]string(nil), req.AtUserIDs...)
	if req.ContentType == AtText {
		var e struct {
			AtUserList []string `json:"atUserList"`
		}
		if json.Unmarshal([]byte(req.Content), &e) == nil {
			raw = append(raw, e.AtUserList...)
		}
	}
	seen := make(map[string]bool, len(raw))
	for _, id := range raw {
		if id == AtAll {
			return []string{"*"}
		}
		seen[id] = true
	}
	// 只留会收到这条通知的人：名单是给收到的人自己用的，别人被 @ 了与他无关。
	var out []string
	for _, id := range recipients {
		if seen[id] {
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
