package api

import (
	"testing"

	"github.com/his1devil/yptd/server/internal/openim"
)

func TestWhoMayRemoveAnAgent(t *testing.T) {
	owner := openim.Member{UserID: "asen", RoleLevel: openim.RoleOwner}
	inviter := openim.Member{UserID: "lina", RoleLevel: openim.RoleOrdinary}
	bystander := openim.Member{UserID: "bob", RoleLevel: openim.RoleOrdinary}
	admin := openim.Member{UserID: "amy", RoleLevel: openim.RoleAdmin}
	bot := openim.Member{UserID: "agentbot", RoleLevel: openim.RoleOrdinary, InviterUserID: "lina"}

	cases := []struct {
		name    string
		who     string
		members []openim.Member
		want    bool
	}{
		{"群主可以", "asen", []openim.Member{owner, bot}, true},
		{"把它拉进来的人可以", "lina", []openim.Member{inviter, bot}, true},
		{"路人不行", "bob", []openim.Member{bystander, bot}, false},
		// 管理员不在规则里：定的是「群主和邀请的可以」，不因为 OpenIM 里管理员能踢普通成员就放行
		{"管理员也不行", "amy", []openim.Member{admin, bot}, false},
		{"不在群里的人不行", "eve", []openim.Member{bot}, false},
		{"agent 不在群里", "asen", []openim.Member{owner}, false},
	}
	for _, c := range cases {
		got, why := mayRemoveAgent(c.who, "agentbot", c.members)
		if got != c.want {
			t.Errorf("%s: got %v (%s)", c.name, got, why)
		}
		if !got && why == "" {
			t.Errorf("%s: 拒绝要给理由，界面上要显示给人看", c.name)
		}
	}
	// 邀请人字段为空（老数据、建群时带进来的）不能被空串的请求者匹配上
	orphan := openim.Member{UserID: "agentbot", InviterUserID: ""}
	if ok, _ := mayRemoveAgent("", "agentbot", []openim.Member{{UserID: ""}, orphan}); ok {
		t.Error("空的邀请人不该匹配任何人")
	}
}
