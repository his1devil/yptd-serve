package api

import (
	"strings"
	"testing"

	"github.com/his1devil/yptd/server/internal/config"
)

var roster = []config.Agent{
	{UserID: "agentbot", Nickname: "助手"},
	{UserID: "agentquant", Nickname: "小盘"},
}

func TestAddressedNeedsAMentionInAGroup(t *testing.T) {
	req := callbackReq{GroupID: "42", AtUserList: []string{"lina"}}
	if got := addressedAgents(req, nil, roster, afterSendGroupMsg); len(got) != 0 {
		t.Fatalf("somebody else was mentioned: %v", got)
	}
	req.AtUserList = append(req.AtUserList, "agentbot")
	if got := addressedAgents(req, nil, roster, afterSendGroupMsg); len(got) != 1 || got[0] != "agentbot" {
		t.Fatalf("got %v", got)
	}
}

func TestOneMessageCanCallOnSeveralAgents(t *testing.T) {
	// 提及顺序是任意的，返回顺序按名册来，这样同一句话在群里
	// 每次都是同样的先后。
	req := callbackReq{GroupID: "42", AtUserList: []string{"agentquant", "lina", "agentbot"}}
	got := addressedAgents(req, nil, roster, afterSendGroupMsg)
	if strings.Join(got, ",") != "agentbot,agentquant" {
		t.Fatalf("got %v", got)
	}
}

func TestADirectMessageToAnAgentIsAlwaysAddressedToIt(t *testing.T) {
	req := callbackReq{SendID: "lina", RecvID: "agentquant"}
	got := addressedAgents(req, nil, roster, afterSendSingleMsg)
	if len(got) != 1 || got[0] != "agentquant" {
		t.Fatalf("a direct message needs no mention: %v", got)
	}
	req.RecvID = "zhang"
	if got := addressedAgents(req, nil, roster, afterSendSingleMsg); len(got) != 0 {
		t.Fatalf("this one was for somebody else: %v", got)
	}
}

func TestConversationIDMatchesWhatOpenIMUses(t *testing.T) {
	if got := conversationID(callbackReq{GroupID: "42"}); got != "sg_42" {
		t.Fatalf("group: %q", got)
	}
	// The pair is sorted, so both directions name the same conversation --
	// which is what a revoke needs.
	a := conversationID(callbackReq{SendID: "lina", RecvID: "agentbot"})
	b := conversationID(callbackReq{SendID: "agentbot", RecvID: "lina"})
	if a != b || a != "si_agentbot_lina" {
		t.Fatalf("direct: %q vs %q", a, b)
	}
}

func TestMentionsInsideTheElementCountToo(t *testing.T) {
	// SDK 发的消息把 @ 放在回调的 atUserList 里，REST 接口发的放在
	// 消息体里。只认一边就会漏掉另一半。
	req := callbackReq{GroupID: "42"}
	got := addressedAgents(req, []string{"agentquant"}, roster, afterSendGroupMsg)
	if len(got) != 1 || got[0] != "agentquant" {
		t.Fatalf("got %v", got)
	}
}

func TestTheTwoMentionListsAreMerged(t *testing.T) {
	req := callbackReq{GroupID: "42", AtUserList: []string{"agentbot"}}
	got := addressedAgents(req, []string{"agentquant"}, roster, afterSendGroupMsg)
	if strings.Join(got, ",") != "agentbot,agentquant" {
		t.Fatalf("got %v", got)
	}
}
