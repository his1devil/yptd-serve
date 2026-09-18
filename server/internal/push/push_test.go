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
	if d.Info.Ex != `{"conversation_id":"si_asen_lina","kind":"message","v":1}` {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
	if d.Info.IOSPushSound != "default" || !d.Info.IOSBadgeCount {
		t.Fatal("sound and badge must be set: the adapter no longer hard-codes them")
	}
}

func TestGroupMessagePushesOnlyTheMentioned(t *testing.T) {
	req := Request{
		UserIDs: []string{"asen", "lina", "bob", "agentbot"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: AtText, AtUserIDs: []string{"asen", "agentbot"},
		Content: `{"text":"@阿森 看一下这个","atUserList":["asen","agentbot"]}`,
	}
	d := Decide(req, fakeNames{}, placeholder)
	if d.Skip {
		t.Fatalf("mentioned people get pushed: %s", d.Reason)
	}
	if len(d.UserIDs) != 1 || d.UserIDs[0] != "asen" {
		t.Fatalf("only the mentioned human: %v", d.UserIDs)
	}
	if d.Info.Title != "#Skywalker" || d.Info.Desc != "Lina：@阿森 看一下这个" {
		t.Fatalf("title/desc = %q / %q", d.Info.Title, d.Info.Desc)
	}
	if !strings.Contains(d.Info.Ex, `"conversation_id":"sg_g1"`) {
		t.Fatalf("ex = %s", d.Info.Ex)
	}
}

func TestGroupMessageWithNoMentionIsNotPushed(t *testing.T) {
	d := Decide(Request{
		UserIDs: []string{"asen", "bob"}, SendID: "lina", GroupID: "g1", SessionType: 3,
		ContentType: Text, Content: text("大家早"),
	}, fakeNames{}, placeholder)
	if !d.Skip {
		t.Fatal("ordinary group chatter must not ring phones")
	}
}

func TestAtAllPushesEveryoneButAgentsAndSender(t *testing.T) {
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
	if d.Info.Ex != `{"conversation_id":"si_agentbot_asen","kind":"run_result","run_id":"run_x","v":1}` {
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
