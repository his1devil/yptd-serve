package bot

import (
	"strings"
	"testing"
	"time"
)

var now = time.Date(2026, 9, 14, 9, 2, 0, 0, time.UTC)

func item() NewsItem {
	return NewsItem{
		ID:        "ab12cd",
		Title:     "OpenAI 开放 Sora 2 API",
		Source:    "OpenAI 博客",
		URL:       "https://openai.com/index/sora-2-api",
		Index:     "https://aihot.news/i/ab12cd",
		Published: now.Add(-2 * time.Hour),
		Summary:   "OpenAI 宣布 Sora 2 开放 API。",
		Take:      "定价压到 Runway 一档以下，视频生成的价格战从这里开始。",
	}
}

func TestDigestOneStoryNeedsNoHeading(t *testing.T) {
	got := Digest([]NewsItem{item()}, now)
	want := "**OpenAI 开放 Sora 2 API**\n" +
		"定价压到 Runway 一档以下，视频生成的价格战从这里开始。\n" +
		"[OpenAI 博客](https://openai.com/index/sora-2-api) · 2 小时前"
	if got != want {
		t.Fatalf("\n got %q\nwant %q", got, want)
	}
}

func TestDigestCitesThePublisherAndNobodyElse(t *testing.T) {
	got := Digest([]NewsItem{item()}, now)
	// 引用位上挂的必须是发布方的名字和它自己的链接。
	if !strings.Contains(got, "[OpenAI 博客](https://openai.com/index/sora-2-api)") {
		t.Fatalf("the publisher should be the citation:\n%s", got)
	}
	// 把稿子捞出来的聚合站一个字都不出现——它不是来源。
	if strings.Contains(got, "aihot") || strings.Contains(got, "AIHOT") {
		t.Fatalf("the aggregator has no place in the citation:\n%s", got)
	}
}

func TestDigestHeadsAndSeparatesSeveral(t *testing.T) {
	second := item()
	second.ID, second.Title, second.Source = "ef34gh", "台积电 2nm 提前量产", "路透"
	second.URL, second.Index = "https://reuters.com/tsmc-2nm", "https://aihot.news/i/ef34gh"
	got := Digest([]NewsItem{item(), second}, now)
	if !strings.HasPrefix(got, "**科技热点** · 2 条\n\n") {
		t.Fatalf("several stories should get a heading with a count:\n%s", got)
	}
	// 条与条之间靠空行分组——渲染器不认列表，空行是唯一的分组手段。
	if strings.Count(got, "\n\n") != 2 {
		t.Fatalf("stories should be separated by a blank line:\n%q", got)
	}
}

func TestDigestFallsBackToTheSummary(t *testing.T) {
	it := item()
	it.Take = ""
	if !strings.Contains(Digest([]NewsItem{it}, now), it.Summary) {
		t.Fatal("with no take, the summary should stand in rather than leaving a bare link")
	}
}

func TestDigestKeepsAMultilineTakeToOneLine(t *testing.T) {
	it := item()
	it.Take = "第一句是判断。\n\n后面模型又絮叨了两段。"
	got := Digest([]NewsItem{it}, now)
	if strings.Contains(got, "絮叨") {
		t.Fatalf("a rambling take should be cut to its first line:\n%s", got)
	}
	if lines := strings.Count(got, "\n"); lines != 2 {
		t.Fatalf("one story is three lines, got %d newlines:\n%s", lines, got)
	}
}

func TestDigestSurvivesMissingPieces(t *testing.T) {
	got := Digest([]NewsItem{{Title: "只有个标题"}}, now)
	if got != "**只有个标题**" {
		t.Fatalf("an item with nothing else should still render its headline, got %q", got)
	}
	if Digest([]NewsItem{{Source: "路透", URL: "https://x"}}, now) != "" {
		t.Fatal("an item with no headline is not a story")
	}
	if Digest(nil, now) != "" {
		t.Fatal("nothing new should produce no message")
	}
}

func TestSinceIsCoarse(t *testing.T) {
	for _, c := range []struct {
		ago  time.Duration
		want string
	}{
		{30 * time.Second, "刚刚"},
		{-time.Minute, "刚刚"}, // 源站时钟跑快了
		{20 * time.Minute, "20 分钟前"},
		{5 * time.Hour, "5 小时前"},
	} {
		if got := since(now.Add(-c.ago), now); got != c.want {
			t.Fatalf("%v ago should read %q, got %q", c.ago, c.want, got)
		}
	}
	if got := since(now.Add(-72*time.Hour), now); !strings.Contains(got, "月") {
		t.Fatalf("anything older than a day should get a date, got %q", got)
	}
	if since(time.Time{}, now) != "" {
		t.Fatal("an unknown publish time should say nothing rather than guess")
	}
}
