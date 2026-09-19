package push

import (
	"encoding/json"
	"strings"
	"testing"
)

type fakeNames struct{}

func (fakeNames) Nickname(id string) string {
	return map[string]string{"lina": "Lina", "asen": "阿森", "agentbot": "HALX"}[id]
}
func (fakeNames) GroupName(id string) string {
	return map[string]string{"g1": "Skywalker"}[id]
}
func (fakeNames) Avatar(id string) string {
	return map[string]string{"lina": "https://im/object/lina.jpg"}[id]
}
func (fakeNames) IsAgent(id string) bool { return strings.HasPrefix(id, "agent") }

const placeholder = "⏳ 正在处理…"

func text(s string) string {
	b, _ := json.Marshal(map[string]string{"content": s})
	return string(b)
}

func TestDirectMessageIsPushedWithSenderAndPreview(t *testing.T) {
	d := Decide(Request{
		UserIDs: []string{"asen"}, SendID: "lina", SessionType: 1, ContentType: Text,
		Content: text("晚上一起吃饭？"),
	}, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("a direct message must be pushed: %s", d.Reason)
	}
	if d.UserIDs != nil {
		// OpenIM 单聊那条路径把名单指针传的是 nil，回名单会让 openim-push 空指针崩掉。
		t.Fatal("never return a user list for a direct message")
	}
	if d.Info.Title != "Lina" || d.Info.Desc != "晚上一起吃饭？" {
		t.Fatalf("title/desc = %q / %q", d.Info.Title, d.Info.Desc)
	}
	if d.Info.Ex != `{"avatar":"https://im/object/lina.jpg","conversation_id":"si_asen_lina","from":"lina","kind":"message","name":"Lina","v":1}` {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
	if d.Info.IOSPushSound != "default" || !d.Info.IOSBadgeCount {
		t.Fatal("sound and badge must be set: the adapter no longer hard-codes them")
	}
}

func TestOrdinaryGroupChatterIsPushedWithNoAtList(t *testing.T) {
	// 2026-09-19 起群里普通发言也推。
	d := Decide(Request{
		UserIDs: []string{"asen", "bob"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: Text, Content: text("大家早"),
	}, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("ordinary group chatter is pushed now: %s", d.Reason)
	}
	if d.Info.Title != "#Skywalker" || d.Info.Desc != "Lina：大家早" {
		t.Fatalf("title/desc = %q / %q", d.Info.Title, d.Info.Desc)
	}
	if strings.Contains(d.Info.Ex, `"at"`) {
		t.Fatalf("nobody was mentioned, ex should carry no at: %s", d.Info.Ex)
	}
}

func TestEveryoneIsPushedAndTheMentionedGoIntoEx(t *testing.T) {
	// 一条群消息只有一份文案，没法对被 @ 的人单独说「提到了你」，所以 @ 到谁写进 ex，
	// 手机端知道自己是谁，展示时自己加重。
	d := Decide(Request{
		UserIDs: []string{"asen", "lina", "bob", "agentbot"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: AtText, AtUserIDs: []string{"asen", "agentbot"},
		Content: `{"text":"@阿森 看一下这个","atUserList":["asen","agentbot"]}`,
	}, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("group messages are pushed now: %s", d.Reason)
	}
	if strings.Join(d.UserIDs, ",") != "asen,bob" {
		t.Fatalf("everyone but the sender and the agents: %v", d.UserIDs)
	}
	if d.Info.Desc != "Lina：@阿森 看一下这个" {
		t.Fatalf("desc = %q", d.Info.Desc)
	}
	// agentbot 被 @ 了但收不到通知，不进 at；bob 收得到但没被 @，也不进。
	if !strings.Contains(d.Info.Ex, `"at":["asen"]`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
}

func TestMentionsInsideTheElementCountToo(t *testing.T) {
	// 管理接口发的 @ 消息，头上的 atUserIDList 是空的，名单只在元素里。
	d := Decide(Request{
		UserIDs: []string{"asen", "bob"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: AtText, AtUserIDs: nil,
		Content: `{"text":"@阿森 看一下","atUserList":["asen"]}`,
	}, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("%s", d.Reason)
	}
	if !strings.Contains(d.Info.Ex, `"at":["asen"]`) {
		t.Fatalf("mention inside the element must count: %s", d.Info.Ex)
	}
}

func TestAtAllMarksEveryoneInEx(t *testing.T) {
	d := Decide(Request{
		UserIDs: []string{"asen", "lina", "bob", "agentbot"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: AtText, AtUserIDs: []string{AtAll}, Content: `{"text":"@所有人 开会"}`,
	}, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatal(d.Reason)
	}
	if strings.Join(d.UserIDs, ",") != "asen,bob" {
		t.Fatalf("got %v", d.UserIDs)
	}
	if !strings.Contains(d.Info.Ex, `"at":["*"]`) {
		t.Fatalf("@所有人 should mark everyone: %s", d.Info.Ex)
	}
}

func TestAgentAnswersArePushedButBroadcastsAreNot(t *testing.T) {
	// agent 在群里说话分两种。回答（有人 @ 它问了）带 run_result 标记，该响手机；
	// 行情、新闻、配置变更是定时发的，不该响——每天几十条会让人把整个 app 静音。
	answer := Request{
		UserIDs: []string{"asen", "bob"}, SendID: "agentbot", GroupID: "g1", SessionType: 3,
		ContentType: Text, Content: text("NVDA 今天 -3.2%，因为…"),
		Ex: `{"v":1,"conversation_id":"sg_g1","kind":"run_result","run_id":"run_x"}`,
	}
	d := Decide(answer, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("an agent answering a question must be pushed: %s", d.Reason)
	}
	if d.Info.Desc != "HALX：NVDA 今天 -3.2%，因为…" {
		t.Fatalf("desc = %q", d.Info.Desc)
	}
	if !strings.Contains(d.Info.Ex, `"kind":"run_result"`) || !strings.Contains(d.Info.Ex, `"run_id":"run_x"`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}

	broadcast := answer
	broadcast.Ex = ""
	broadcast.Content = text("📈 NVDA 涨了 3%")
	if got := Decide(broadcast, fakeNames{}, placeholder); !got.Skip || got.Reason != "agent broadcast" {
		t.Fatalf("an unprompted agent broadcast must not ring phones: skip=%v %s", got.Skip, got.Reason)
	}
	// 私聊里 agent 说什么都是在回答，照推
	direct := broadcast
	direct.GroupID, direct.SessionType, direct.UserIDs = "", 1, []string{"asen"}
	if got := Decide(direct, fakeNames{}, placeholder); got.Skip {
		t.Fatalf("an agent in a direct chat is always answering: %s", got.Reason)
	}
}

func TestGroupListIsOmittedWhenUnchanged(t *testing.T) {
	// 名单没变就不回名单，少一处让 OpenIM 换名单的机会。
	d := Decide(Request{
		UserIDs: []string{"asen"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: AtText, AtUserIDs: []string{"asen"}, Content: `{"text":"@阿森 在？"}`,
	}, fakeNames{}, placeholder)
	if d.Skip || d.UserIDs != nil {
		t.Fatalf("skip=%v ids=%v", d.Skip, d.UserIDs)
	}
}

func TestThingsThatNeverPush(t *testing.T) {
	cases := map[string]Request{
		"reaction":     {UserIDs: []string{"asen"}, SendID: "agentbot", SessionType: 1, ContentType: Custom, Content: `{"data":"{\"yptd\":\"reaction\"}","description":"reaction"}`},
		"typing":       {UserIDs: []string{"asen"}, SendID: "lina", SessionType: 1, ContentType: Typing},
		"notification": {UserIDs: []string{"asen"}, SendID: "lina", SessionType: 1, ContentType: 1201},
		"placeholder":  {UserIDs: []string{"asen"}, SendID: "agentbot", SessionType: 1, ContentType: Text, Content: text(placeholder)},
		"only agents":  {UserIDs: []string{"agentbot"}, SendID: "lina", SessionType: 1, ContentType: Text, Content: text("hi")},
		"only sender":  {UserIDs: []string{"lina"}, SendID: "lina", GroupID: "g1", SessionType: 3, ContentType: Text, Content: text("自言自语")},
	}
	for name, req := range cases {
		if d := Decide(req, fakeNames{}, placeholder); !d.Skip {
			t.Errorf("%s should be skipped", name)
		}
	}
	// 人发的同一句话不是占位，照推
	human := Request{UserIDs: []string{"asen"}, SendID: "lina", SessionType: 1, ContentType: Text, Content: text(placeholder)}
	if Decide(human, fakeNames{}, placeholder).Skip {
		t.Error("a human typing the placeholder text is still a message")
	}
}

func TestRunResultKindSurvivesFromTheSendersEx(t *testing.T) {
	// 发送方（Go 服务）知道这是一次 run 的结果，回调补不出来，得留住；会话 id 不信它的。
	d := Decide(Request{
		UserIDs: []string{"asen"}, SendID: "agentbot", SessionType: 1, ContentType: Text, Content: text("结论是…"),
		Ex: `{"v":1,"conversation_id":"wrong","kind":"run_result","run_id":"run_x"}`,
	}, fakeNames{}, placeholder)
	if !strings.Contains(d.Info.Ex, `"conversation_id":"si_agentbot_asen"`) ||
		!strings.Contains(d.Info.Ex, `"kind":"run_result"`) ||
		!strings.Contains(d.Info.Ex, `"run_id":"run_x"`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
}

func TestPreviewPerContentType(t *testing.T) {
	cases := []struct {
		ct   int32
		body string
		want string
	}{
		{Text, text("第一行\n第二行"), "第一行 第二行"},
		{Picture, `{"sourcePicture":{"url":"x"}}`, "[图片]"},
		{Video, `{}`, "[视频]"},
		{File, `{"fileName":"设计稿.pdf"}`, "[文件] 设计稿.pdf"},
		{Quote, `{"text":"同意","quoteMessage":{}}`, "同意"},
		{Text, text(""), "[消息]"},
		{999, `{}`, "[消息]"},
	}
	for _, c := range cases {
		if got := Preview(c.ct, c.body); got != c.want {
			t.Errorf("Preview(%d) = %q, want %q", c.ct, got, c.want)
		}
	}
	long := strings.Repeat("长", 100)
	if got := Preview(Text, text(long)); len([]rune(got)) != 80 || !strings.HasSuffix(got, "…") {
		t.Errorf("long preview should be cut to 80 runes with an ellipsis, got %d", len([]rune(got)))
	}
}

func TestUnknownNamesFallBack(t *testing.T) {
	d := Decide(Request{
		UserIDs: []string{"asen"}, SendID: "stranger", GroupID: "g-unknown", SessionType: 3,
		ContentType: AtText, AtUserIDs: []string{"asen"}, Content: `{"text":"@阿森"}`,
	}, fakeNames{}, placeholder)
	if d.Info.Title != "#群聊" || !strings.HasPrefix(d.Info.Desc, "stranger：") {
		t.Fatalf("title/desc = %q / %q", d.Info.Title, d.Info.Desc)
	}
}

func TestPreviewStripsMarkdown(t *testing.T) {
	// agent 的回答几乎都是 markdown。不剥的话锁屏上全是表格分隔线和星号，
	// 80 个字符里没几个是有效内容。
	cases := []struct{ in, want string }{
		{"## 结论\n\n**NVDA.US** 今天 `-3.2%`", "结论 NVDA.US 今天 -3.2%"},
		{"| 标的 | 涨跌 |\n|---|---|\n| NVDA | +3.2% |", "标的 涨跌 NVDA +3.2%"},
		{"看这段：\n```go\nfunc main() {}\n```\n就是它", "看这段： [代码] 就是它"},
		{"- 第一条\n- 第二条", "第一条 第二条"},
		{"> 他说过\n\n---\n\n我同意", "他说过 我同意"},
		{"见 [文档](https://x.com/a_b) 第三节", "见 文档 第三节"},
		{"![截图](https://x/a.png) 你看", "[图片] 你看"},
		{"~~算了~~ 改主意了", "算了 改主意了"},
		{"user_id 和 order_id 对不上", "user_id 和 order_id 对不上"}, // 单下划线不能动
		{"2 * 3 = 6", "2 * 3 = 6"},                           // 落单的星号不是强调
	}
	for _, c := range cases {
		if got := Preview(Text, text(c.in)); got != c.want {
			t.Errorf("Preview(%q)\n  = %q\nwant %q", c.in, got, c.want)
		}
	}
}

func TestNamesAreCleanedBeforeTheyReachTheTitle(t *testing.T) {
	// 昵称中间的换行躲得过注册时的 TrimSpace；群名服务端根本没校验过。
	// 原样拼进标题就是断成两行的名字，真正的内容被挤没。
	if got := name("阿\n森"); got != "阿 森" {
		t.Errorf("name = %q", got)
	}
	if got := name(strings.Repeat("长", 40)); len([]rune(got)) != 16 {
		t.Errorf("长昵称要截断，得到 %d 个字", len([]rune(got)))
	}
}

func TestCutDoesNotSliceEmojiApart(t *testing.T) {
	// 一个看得见的 emoji 常常不止一个 rune，切在中间会留下色块或字母方框。
	for _, e := range []string{"👨‍👩‍👧‍👦", "🇨🇳", "👍🏻", "é"} {
		s := strings.Repeat("字", 79) + e + "尾巴"
		got := cut(s, 80)
		if strings.HasSuffix(strings.TrimSuffix(got, "…"), "‍") {
			t.Errorf("%q: 结尾留了零宽连接符", e)
		}
		r := []rune(strings.TrimSuffix(got, "…"))
		if n := len(r); n > 0 && r[n-1] >= 0x1F1E6 && r[n-1] <= 0x1F1FF {
			t.Errorf("%q: 结尾留了半面国旗", e)
		}
		if n := len(r); n > 0 && r[n-1] >= 0x1F3FB && r[n-1] <= 0x1F3FF {
			t.Errorf("%q: 结尾留了孤立的肤色", e)
		}
	}
}

func TestExCarriesTheMessageID(t *testing.T) {
	// 手机靠 msg_id 精确等待并高亮那一条；没有它，从通知进会话只能看到旧消息，
	// 新的那条 0.5 秒后才追加进来（2026-09-18 真机观测到）。
	d := Decide(Request{
		UserIDs: []string{"asen"}, RecvID: "asen", SendID: "lina", SessionType: 1,
		ContentType: Text, Content: text("在吗"), ClientMsgID: "cmid-7",
	}, fakeNames{}, placeholder)
	if !strings.Contains(d.Info.Ex, `"msg_id":"cmid-7"`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
	if !strings.Contains(d.Info.Ex, `"conversation_id":"si_asen_lina"`) {
		t.Fatalf("会话 id 该用 recvID 算：%s", d.Info.Ex)
	}
}

func TestExCarriesTheSendersAvatarWhenThereIsOne(t *testing.T) {
	// 手机在通知里画发送者的头像；没设头像的人就留空，手机退回 app 自己的标记。
	d := Decide(Request{
		UserIDs: []string{"asen"}, RecvID: "asen", SendID: "lina", SessionType: 1,
		ContentType: Text, Content: text("在吗"),
	}, fakeNames{}, placeholder)
	if !strings.Contains(d.Info.Ex, `"avatar":"https://im/object/lina.jpg"`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
	none := Decide(Request{
		UserIDs: []string{"lina"}, RecvID: "lina", SendID: "asen", SessionType: 1,
		ContentType: Text, Content: text("在"),
	}, fakeNames{}, placeholder)
	if strings.Contains(none.Info.Ex, "avatar") {
		t.Fatalf("没设头像就不该有这个字段：%s", none.Info.Ex)
	}
}

func TestExCarriesTheSenderAndRoomSeparately(t *testing.T) {
	// iOS 的通知扩展要把发送者和群名分开填进 INSendMessageIntent 才能画大头像；
	// 从「#群名」「张三：内容」这种拼好的字符串里往回拆是在跟格式较劲。
	d := Decide(Request{
		UserIDs: []string{"asen", "bob"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: Text, Content: text("明天开会"),
	}, fakeNames{}, placeholder)
	for _, want := range []string{`"from":"lina"`, `"name":"Lina"`, `"room":"Skywalker"`} {
		if !strings.Contains(d.Info.Ex, want) {
			t.Errorf("ex 里少了 %s：%s", want, d.Info.Ex)
		}
	}
	// 私聊没有群名
	direct := Decide(Request{
		UserIDs: []string{"asen"}, RecvID: "asen", SendID: "lina", SessionType: 1,
		ContentType: Text, Content: text("在吗"),
	}, fakeNames{}, placeholder)
	if strings.Contains(direct.Info.Ex, `"room"`) {
		t.Errorf("私聊不该有 room：%s", direct.Info.Ex)
	}
}
