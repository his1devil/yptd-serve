package api

import (
	"strings"
	"testing"

	"github.com/his1devil/yptd/server/internal/config"
)

func TestCleanSymbols(t *testing.T) {
	// 长桥的代码是大写的；人手打的 nvda.us 不该变成一个查不到的标的
	got := cleanSymbols([]string{" nvda.us ", "700.HK", "", "NVDA.US", "  "})
	if strings.Join(got, ",") != "NVDA.US,700.HK" {
		t.Fatalf("要去空白、去重、统一大写，得到 %v", got)
	}
}

func TestCheckWatchRejectsWhatWouldSilentlyNeverMatch(t *testing.T) {
	// 不带市场后缀的代码查不到任何行情，但看着像配好了——静默地永远不推
	if msg := checkWatch(config.Watch{Symbols: []string{"NVDA"}}); msg == "" {
		t.Fatal("没有市场后缀的代码应该当场拒绝")
	}
	if msg := checkWatch(config.Watch{Symbols: []string{"NVDA.US"}, Threshold: 3}); msg != "" {
		t.Fatalf("正常的配置不该被拦：%s", msg)
	}
}

func TestCheckWatchBoundsTheNoisyKnobs(t *testing.T) {
	if checkWatch(config.Watch{Threshold: 99}) == "" {
		t.Fatal("阈值超出范围要拦")
	}
	if checkWatch(config.Watch{Every: "两分钟"}) == "" {
		t.Fatal("写法不对的间隔要拦")
	}
	if got := checkWatch(config.Watch{Symbols: make([]string, 51)}); got == "" {
		t.Fatal("标的太多要拦")
	}
}

func TestSummaryReadsLikeASentence(t *testing.T) {
	// 这句话会原样发到群里，是配置的可读快照
	got := watchSummary(config.Watch{Symbols: []string{"NVDA.US", "700.HK"}, Threshold: 3, Every: "2m0s"})
	for _, want := range []string{"NVDA.US", "700.HK", "3%", "2m0s"} {
		if !strings.Contains(got, want) {
			t.Fatalf("摘要里该有 %q：%s", want, got)
		}
	}
	if got := watchSummary(config.Watch{}); !strings.Contains(got, "不盯") {
		t.Fatalf("清空之后也要说得明白：%s", got)
	}
}

func TestOnlyAgentsWithPushHaveAConfigPage(t *testing.T) {
	// HALX 这类没有推送能力的，配置页根本不该出现，而不是给一张空表单
	if _, ok := formFor(config.Agent{UserID: "agentbot"}); ok {
		t.Fatal("没有推送能力的 agent 不该有配置页")
	}
	if f, ok := formFor(config.Agent{UserID: "agentjomo", News: config.News{Feed: "aihot"}}); !ok || f.Kind != "news" {
		t.Fatalf("有新闻源的该拿到新闻表单：%v %v", ok, f.Kind)
	}
	if f, ok := formFor(config.Agent{UserID: "agentcharlie"}); !ok || f.Kind != "watch" {
		t.Fatalf("行情 agent 该拿到盯盘表单：%v %v", ok, f.Kind)
	}
}

func TestEveryFieldExplainsItself(t *testing.T) {
	// 通用表单写不出针对性的解释，所以每个字段都得自带一句——
	// 一个配 1% 阈值的人需要有人告诉他那会很吵
	for _, f := range append(append([]field{}, watchForm.Fields...), newsForm.Fields...) {
		if f.Hint == "" {
			t.Fatalf("%s 没有 hint", f.Key)
		}
	}
}
