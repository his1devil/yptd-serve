package bot

import (
	"log/slog"
	"strings"
	"testing"

	"github.com/his1devil/yptd/server/internal/config"
)

func testBot() *Bot {
	return New(Config{Agents: []config.Agent{
		{UserID: "agentbot", Nickname: "助手"},
		{UserID: "agentquant", Nickname: "小盘", Opencode: "quant", Model: "zhipuai/glm-5.3"},
	}}, nil, nil, nil, nil, slog.New(slog.DiscardHandler))
}

func TestWantsIgnoresItsOwnMessages(t *testing.T) {
	// The bot's replies come back through the same webhook. Answering them
	// is an unbounded loop that spends real money.
	b := testBot()
	own := Message{SenderID: "agentbot", AgentID: "agentbot", ContentType: 101, Text: "上一条回答"}
	if b.Wants(own) {
		t.Fatal("bot must never answer itself")
	}
}

func TestWantsOnlyWhenAddressed(t *testing.T) {
	b := testBot()
	base := Message{SenderID: "lina", ContentType: 106, Text: "帮我看看"}
	if b.Wants(base) {
		t.Fatal("an unaddressed group message is not the bot's business")
	}
	addressed := base
	addressed.AgentID = "agentbot"
	if !b.Wants(addressed) {
		t.Fatal("a mention should be answered")
	}
}

func TestWantsSkipsEmptyAndUnsupportedKinds(t *testing.T) {
	b := testBot()
	for _, m := range []Message{
		{SenderID: "lina", AgentID: "agentbot", ContentType: 101, Text: "   "},
		{SenderID: "lina", AgentID: "agentbot", ContentType: 102, Text: "一张图"},
	} {
		if b.Wants(m) {
			t.Fatalf("should have been skipped: %+v", m)
		}
	}
}

func TestParseContentStripsTheMentionSoTheModelSeesTheQuestion(t *testing.T) {
	content := `{"text":"@助手 这个仓库是干什么的","atUserList":["agentbot"],` +
		`"atUsersInfo":[{"atUserID":"agentbot","groupNickname":"助手"}]}`
	text, mentions := ParseContent(106, content, []string{"助手", "小盘"})
	if text != "这个仓库是干什么的" {
		t.Fatalf("mention not stripped: %q", text)
	}
	if len(mentions) != 1 || mentions[0] != "agentbot" {
		t.Fatalf("mentions: %v", mentions)
	}
}

func TestParseContentReadsPlainText(t *testing.T) {
	text, mentions := ParseContent(101, `{"content":"直接问一句"}`, []string{"助手"})
	if text != "直接问一句" {
		t.Fatalf("text: %q", text)
	}
	if len(mentions) != 0 {
		t.Fatalf("plain text mentions nobody: %v", mentions)
	}
}

func TestParseContentSurvivesRubbish(t *testing.T) {
	if text, _ := ParseContent(101, "not json at all", []string{"助手"}); text != "" {
		t.Fatalf("expected empty, got %q", text)
	}
}

func TestAnswerTextDropsTheReasoning(t *testing.T) {
	// GLM's reasoning is routinely longer than its answer; posting it would
	// bury the answer in thinking nobody asked for.
	parts := []part{
		{Type: "step-start"},
		{Type: "reasoning", Text: "The user is asking..."},
		{Type: "text", Text: "  珠峰高约 8848.86 米。  "},
		{Type: "step-finish"},
	}
	if got := answerText(parts); got != "珠峰高约 8848.86 米。" {
		t.Fatalf("got %q", got)
	}
}

func TestAnswerTextJoinsSeveralTextParts(t *testing.T) {
	parts := []part{{Type: "text", Text: "第一段"}, {Type: "text", Text: "第二段"}}
	if got := answerText(parts); !strings.Contains(got, "第一段") || !strings.Contains(got, "第二段") {
		t.Fatalf("got %q", got)
	}
}

func TestOneLineFlattensAndTruncates(t *testing.T) {
	long := strings.Repeat("很长", 400)
	got := oneLine("第一行\n第二行")
	if strings.Contains(got, "\n") {
		t.Fatal("newlines must go")
	}
	if len([]rune(oneLine(long))) > 320 {
		t.Fatal("a stack trace must not become the whole reply")
	}
}

func TestOneAgentNeverAnswersAnother(t *testing.T) {
	// 两个 agent 在同一个群里，其中一个的回答会带着 @ 回到 webhook。
	// 谁都不许接这一棒，否则两个模型会一直互相回下去。
	b := testBot()
	m := Message{SenderID: "agentquant", AgentID: "agentbot", ContentType: 101, Text: "刚才那段"}
	if b.Wants(m) {
		t.Fatal("一个 agent 不能回另一个 agent")
	}
}

func TestWantsRejectsAnAgentItDoesNotHave(t *testing.T) {
	b := testBot()
	m := Message{SenderID: "lina", AgentID: "agentghost", ContentType: 101, Text: "在吗"}
	if b.Wants(m) {
		t.Fatal("名册里没有这个 agent，不该应答")
	}
}

func TestParseContentStripsEveryAgentName(t *testing.T) {
	// 一句话点了两个 agent，各自看到的都该是干净的问题本身。
	content := `{"text":"@助手 @小盘 这支票怎么看","atUserList":["agentbot","agentquant"],` +
		`"atUsersInfo":[{"atUserID":"agentbot","groupNickname":"助手"},` +
		`{"atUserID":"agentquant","groupNickname":"小盘"}]}`
	text, mentions := ParseContent(106, content, []string{"助手", "小盘"})
	if text != "这支票怎么看" {
		t.Fatalf("没剥干净: %q", text)
	}
	if len(mentions) != 2 {
		t.Fatalf("mentions: %v", mentions)
	}
}

func TestNicknamesFollowTheRoster(t *testing.T) {
	if got := testBot().Nicknames(); len(got) != 2 || got[0] != "助手" || got[1] != "小盘" {
		t.Fatalf("got %v", got)
	}
}

func TestReactionPayloadMatchesTheClientProtocol(t *testing.T) {
	// 客户端的 parseReaction 认 {yptd:"reaction", target, emoji}，少一个字段就静默丢弃。
	got := reactionData("c1", "👌")
	for _, want := range []string{`"yptd":"reaction"`, `"target":"c1"`, `"emoji":"👌"`} {
		if !strings.Contains(got, want) {
			t.Fatalf("payload %s lacks %s", got, want)
		}
	}
}
