package api

import (
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
)

// 这两个 before 钩子是「不允许被拉进群」唯一真正生效的地方：客户端拿自己的 token
// 直连 OpenIM 建群和拉人，只有 OpenIM 先来问一句，这个开关才是规则而不是请求。

type dir map[string]struct {
	name     string
	joinable bool
}

func (d dir) lookup(id string) (string, bool, bool) {
	u, ok := d[id]
	return u.name, u.joinable, ok
}

func agentsAre(ids ...string) func(string) bool {
	set := map[string]bool{}
	for _, id := range ids {
		set[id] = true
	}
	return func(id string) bool { return set[id] }
}

var people = dir{
	"shy":  {"小声", false},
	"open": {"开放", true},
}

func none(string) bool { return false }

func TestRefusesSomeoneWhoDidNotOptIn(t *testing.T) {
	got := refusedBy([]string{"shy"}, none, people.lookup)
	if len(got) != 1 || got[0] != "小声" {
		t.Fatalf("没开放被拉的人要被挡住，而且要说清是谁：%v", got)
	}
}

func TestLetsAnOptedInPersonThrough(t *testing.T) {
	if got := refusedBy([]string{"open"}, none, people.lookup); len(got) != 0 {
		t.Fatalf("开了开关的人应该放行：%v", got)
	}
}

func TestRefusesTheWholeBatch(t *testing.T) {
	// OpenIM 上游处理 RefusedMembersAccount 的代码是注释掉的，没法只拒一个。
	// 客户端要自己先过滤，这里是防改客户端的兜底。
	if got := refusedBy([]string{"open", "shy"}, none, people.lookup); len(got) != 1 {
		t.Fatalf("混了一个不允许的，整批都要拦：%v", got)
	}
}

func TestAgentsAreNotSubjectToThisSwitch(t *testing.T) {
	// agent 是被派来干活的，没有「愿不愿意进群」这回事
	d := dir{"agentjomo": {"JOMO", false}}
	if got := refusedBy([]string{"agentjomo"}, agentsAre("agentjomo"), d.lookup); len(got) != 0 {
		t.Fatalf("agent 不该被这套开关挡住：%v", got)
	}
}

func TestUnknownAccountsPass(t *testing.T) {
	// 这道门是挡「明确说了不要」的人，不是挡所有查不清的情况：
	// 查不到可能是别的系统建的账号，也可能只是数据库一时不通。
	if got := refusedBy([]string{"never-heard-of"}, none, people.lookup); len(got) != 0 {
		t.Fatalf("查不清的应该放行：%v", got)
	}
}

func TestTheSamePersonIsNamedOnce(t *testing.T) {
	if got := refusedBy([]string{"shy", "shy", ""}, none, people.lookup); len(got) != 1 {
		t.Fatalf("重复的 id 不该重复报名字：%v", got)
	}
}

func TestFallsBackToTheIdWhenThereIsNoName(t *testing.T) {
	d := dir{"u_x": {"", false}}
	if got := refusedBy([]string{"u_x"}, none, d.lookup); len(got) != 1 || got[0] != "u_x" {
		t.Fatalf("没昵称就报 id，别给一句名字是空的错误：%v", got)
	}
}

// 两条把人放进群的路都要拦：只拦 invite 的话，「建个新群，成员里带上他」照样能把人拖进来。
func TestBothShapesOfRequestYieldTheSamePeople(t *testing.T) {
	var invite, create joinReq
	if err := json.Unmarshal([]byte(`{"groupID":"g1","invitedUserIDs":["shy","open"]}`), &invite); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(`{"groupID":"g2","memberList":[{"userID":"shy"},{"userID":"open"}]}`), &create); err != nil {
		t.Fatal(err)
	}
	if strings.Join(invite.userIDs(beforeInvite), ",") != "shy,open" {
		t.Fatalf("拉人的载荷解错了：%v", invite.userIDs(beforeInvite))
	}
	// 建群那条路上第一个是群主自己，他不算「被别人加进来」
	if strings.Join(create.userIDs(beforeMembersAdd), ",") != "open" {
		t.Fatalf("建群时该跳过群主：%v", create.userIDs(beforeMembersAdd))
	}
}

func TestCreatingYourOwnGroupIsNotBeingAddedToOne(t *testing.T) {
	// 群主自己没开「允许被拉」是常态（默认就是关的）。要是把他也算进来，
	// 谁都建不了群——被自己的房间拒之门外。
	var create joinReq
	if err := json.Unmarshal([]byte(`{"groupID":"g","memberList":[{"userID":"shy"}]}`), &create); err != nil {
		t.Fatal(err)
	}
	if got := create.userIDs(beforeMembersAdd); len(got) != 0 {
		t.Fatalf("只有群主一个人时没人需要检查：%v", got)
	}
	if got := refusedBy(create.userIDs(beforeMembersAdd), none, people.lookup); len(got) != 0 {
		t.Fatalf("建自己的群不该被自己的开关挡住：%v", got)
	}
}

func TestDenyCarriesWhatOpenIMActuallyReads(t *testing.T) {
	// Parse() 认的是 actionCode==0 && nextCode==1，只发 errCode 会被当成放行。
	w := httptest.NewRecorder()
	callbackDeny(w, "不行")
	var out map[string]any
	if err := json.Unmarshal(w.Body.Bytes(), &out); err != nil {
		t.Fatal(err)
	}
	if out["actionCode"] != float64(0) || out["nextCode"] != float64(1) {
		t.Fatalf("少了这两个字段，拒绝就是一句没人听的话：%v", out)
	}
	if out["errCode"] == float64(0) {
		t.Fatalf("errCode 得是非零，客户端要靠它报错：%v", out)
	}
	// OpenIM 把 errMsg 和 errDlt 拼起来当最终文案，两处同一句会读到两遍
	if out["errDlt"] != "" {
		t.Fatalf("errDlt 该留空，否则文案重一遍：%v", out)
	}
}
