package api

import "testing"

func TestAddressedToBotNeedsAMentionInAGroup(t *testing.T) {
	req := callbackReq{GroupID: "42", AtUserList: []string{"lina"}}
	if addressedToBot(req, "agentbot", afterSendGroupMsg) {
		t.Fatal("somebody else was mentioned")
	}
	req.AtUserList = append(req.AtUserList, "agentbot")
	if !addressedToBot(req, "agentbot", afterSendGroupMsg) {
		t.Fatal("the bot was mentioned")
	}
}

func TestADirectMessageToTheBotIsAlwaysAddressedToIt(t *testing.T) {
	req := callbackReq{SendID: "lina", RecvID: "agentbot"}
	if !addressedToBot(req, "agentbot", afterSendSingleMsg) {
		t.Fatal("a direct message needs no mention")
	}
	req.RecvID = "zhang"
	if addressedToBot(req, "agentbot", afterSendSingleMsg) {
		t.Fatal("this one was for somebody else")
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
