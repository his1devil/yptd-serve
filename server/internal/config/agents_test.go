package config

import (
	"os"
	"path/filepath"
	"testing"
)

func write(t *testing.T, body string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "agents.json")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// 只有一个助手的部署不该为了说明这件事去写一个文件。
func TestNoRosterFileMeansOneAgentFromTheEnvironment(t *testing.T) {
	agents, err := LoadAgents("", "agentbot", "HALX", "zhipuai/glm-5.3")
	if err != nil {
		t.Fatal(err)
	}
	if len(agents) != 1 || agents[0].UserID != "agentbot" || agents[0].Nickname != "HALX" {
		t.Fatalf("got %+v", agents)
	}
	if agents[0].Model != "zhipuai/glm-5.3" {
		t.Fatalf("model: %q", agents[0].Model)
	}
}

func TestRosterKeepsOrderAndFillsInTheDefaultModel(t *testing.T) {
	path := write(t, `[
	  {"user_id":"agentbot","nickname":"HALX","opencode":"halx"},
	  {"user_id":"agentquant","nickname":"小盘","opencode":"quant","model":"zhipuai/glm-4.6","tag":"行情"}
	]`)
	agents, err := LoadAgents(path, "unused", "unused", "zhipuai/glm-5.3")
	if err != nil {
		t.Fatal(err)
	}
	if len(agents) != 2 {
		t.Fatalf("got %d", len(agents))
	}
	// 没写 model 的继承默认值，写了的保持自己的。
	if agents[0].Model != "zhipuai/glm-5.3" || agents[1].Model != "zhipuai/glm-4.6" {
		t.Fatalf("models: %q %q", agents[0].Model, agents[1].Model)
	}
	if agents[1].Tag != "行情" {
		t.Fatalf("tag: %q", agents[1].Tag)
	}
}

func TestDuplicateNicknameIsRefused(t *testing.T) {
	// @ 是按名字点的，两个 agent 同名就没法判断叫的是谁。
	path := write(t, `[
	  {"user_id":"a","nickname":"助手"},
	  {"user_id":"b","nickname":"助手"}
	]`)
	if _, err := LoadAgents(path, "", "", ""); err == nil {
		t.Fatal("重名必须报错")
	}
}

func TestDuplicateUserIDIsRefused(t *testing.T) {
	path := write(t, `[
	  {"user_id":"a","nickname":"甲"},
	  {"user_id":"a","nickname":"乙"}
	]`)
	if _, err := LoadAgents(path, "", "", ""); err == nil {
		t.Fatal("重复 user_id 必须报错")
	}
}

func TestIncompleteAgentIsRefused(t *testing.T) {
	for _, body := range []string{
		`[{"user_id":"a"}]`,
		`[{"nickname":"甲"}]`,
		`[]`,
		`不是 json`,
	} {
		if _, err := LoadAgents(write(t, body), "", "", ""); err == nil {
			t.Fatalf("应该报错: %s", body)
		}
	}
}

func TestMissingRosterFileIsAnError(t *testing.T) {
	// 配了路径却读不到，说明部署错了；这时候悄悄退回单 agent 会让人
	// 以为名册生效了，其实没有。
	if _, err := LoadAgents("/nonexistent/agents.json", "agentbot", "HALX", ""); err == nil {
		t.Fatal("文件不存在必须报错")
	}
}

func TestColoursAreAssignedByPositionSoTheyNeverCollide(t *testing.T) {
	// 四个 agent 哈希到六个颜色里，撞色的概率高得离谱——实测 HALX 和
	// Linus 同蓝、Charlie 和 Jobs 同紫，一排四个只剩两种颜色。
	path := write(t, `[
	  {"user_id":"agentbot","nickname":"HALX"},
	  {"user_id":"agentcharlie","nickname":"Charlie"},
	  {"user_id":"agentlinus","nickname":"Linus"},
	  {"user_id":"agentjobs","nickname":"Jobs"}
	]`)
	agents, err := LoadAgents(path, "", "", "")
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]string{}
	for _, a := range agents {
		if a.Color == "" {
			t.Fatalf("%s 没分到颜色", a.UserID)
		}
		if other, dup := seen[a.Color]; dup {
			t.Fatalf("%s 和 %s 撞色了 (%s)", other, a.UserID, a.Color)
		}
		seen[a.Color] = a.UserID
	}
}

func TestAnExplicitColourIsKept(t *testing.T) {
	path := write(t, `[{"user_id":"a","nickname":"甲","color":"#FF9E80"}]`)
	agents, err := LoadAgents(path, "", "", "")
	if err != nil {
		t.Fatal(err)
	}
	if agents[0].Color != "#FF9E80" {
		t.Fatalf("color: %q", agents[0].Color)
	}
}
