package bot

import (
	"log/slog"
	"strings"
	"testing"
)

func testBot() *Bot {
	return New(Config{UserID: "agentbot", Nickname: "助手"}, nil, nil, nil,
		slog.New(slog.DiscardHandler))
}

func TestWantsIgnoresItsOwnMessages(t *testing.T) {
	// The bot's replies come back through the same webhook. Answering them
	// is an unbounded loop that spends real money.
	b := testBot()
	own := Message{SenderID: "agentbot", Mentioned: true, ContentType: 101, Text: "上一条回答"}
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
	addressed.Mentioned = true
	if !b.Wants(addressed) {
		t.Fatal("a mention should be answered")
	}
}

func TestWantsSkipsEmptyAndUnsupportedKinds(t *testing.T) {
	b := testBot()
	for _, m := range []Message{
		{SenderID: "lina", Mentioned: true, ContentType: 101, Text: "   "},
		{SenderID: "lina", Mentioned: true, ContentType: 102, Text: "一张图"},
	} {
		if b.Wants(m) {
			t.Fatalf("should have been skipped: %+v", m)
		}
	}
}

func TestParseContentStripsTheMentionSoTheModelSeesTheQuestion(t *testing.T) {
	content := `{"text":"@助手 这个仓库是干什么的","atUserList":["agentbot"],` +
		`"atUsersInfo":[{"atUserID":"agentbot","groupNickname":"助手"}]}`
	text, mentions := ParseContent(106, content, "助手")
	if text != "这个仓库是干什么的" {
		t.Fatalf("mention not stripped: %q", text)
	}
	if len(mentions) != 1 || mentions[0] != "agentbot" {
		t.Fatalf("mentions: %v", mentions)
	}
}

func TestParseContentReadsPlainText(t *testing.T) {
	text, mentions := ParseContent(101, `{"content":"直接问一句"}`, "助手")
	if text != "直接问一句" {
		t.Fatalf("text: %q", text)
	}
	if len(mentions) != 0 {
		t.Fatalf("plain text mentions nobody: %v", mentions)
	}
}

func TestParseContentSurvivesRubbish(t *testing.T) {
	if text, _ := ParseContent(101, "not json at all", "助手"); text != "" {
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
