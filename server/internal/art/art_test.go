package art

import (
	"os"
	"path/filepath"
	"testing"
)

var pool = []string{"1-indigo.png", "2-pink.png", "3-sky.png", "4-royal.png", "5-lavender.png", "6-mint.png"}

func TestPickMatchesTheIOSFallback(t *testing.T) {
	// 期望值是拿 iOS 的 AgentArt.name(for:) 同一个算法手算的（FNV-1a 32 位，按 UTF-8
	// 字节，模 6，数组顺序 Indigo/Pink/Sky/Royal/Lavender/Mint）。对不上就意味着没升级
	// 的 iPhone 和服务端给同一个 agent 挑了两张不同的脸。
	cases := map[string]string{}
	for _, id := range []string{"agentnew", "agentquant2", "a", "机器人"} {
		hash := uint32(2166136261)
		for _, b := range []byte(id) {
			hash ^= uint32(b)
			hash *= 16777619
		}
		cases[id] = pool[hash%6]
	}
	for id, want := range cases {
		if got := Pick(id, pool); got != want {
			t.Errorf("Pick(%q) = %s, want %s", id, got, want)
		}
	}
	if Pick("agentnew", pool) != Pick("agentnew", pool) {
		t.Error("同一个 id 必须永远挑到同一张")
	}
	if Pick("x", nil) != "" {
		t.Error("空池子挑不出东西")
	}
}

func TestResolve(t *testing.T) {
	dir := "/etc/art"
	if s := Resolve("https://x/a.png", "agentbot", dir, pool); s.URL != "https://x/a.png" || s.File != "" {
		t.Errorf("公网地址原样用：%+v", s)
	}
	if s := Resolve("dummy.png", "agentdummy", dir, pool); s.File != "/etc/art/dummy.png" {
		t.Errorf("相对路径相对 art 目录：%+v", s)
	}
	if s := Resolve("/abs/x.png", "a", dir, pool); s.File != "/abs/x.png" {
		t.Errorf("绝对路径原样：%+v", s)
	}
	if s := Resolve("", "agentnew", dir, pool); s.File != Pick("agentnew", pool) {
		t.Errorf("没填就从池子里挑：%+v", s)
	}
}

func TestObjectNameCarriesTheContentHash(t *testing.T) {
	// 换了图就是换了地址，任何一端都不会从缓存里继续显示旧脸。
	got := ObjectName("agentbot", "/etc/art/pool/1-indigo.PNG", "0123456789abcdef0123")
	if got != "agentbot/avatar-0123456789ab.png" {
		t.Errorf("got %s", got)
	}
}

func TestPoolIsSortedAndSkipsJunk(t *testing.T) {
	dir := t.TempDir()
	os.MkdirAll(filepath.Join(dir, "pool"), 0o755)
	for _, n := range []string{"2-pink.png", "1-indigo.png", ".DS_Store", "notes.txt"} {
		os.WriteFile(filepath.Join(dir, "pool", n), []byte("x"), 0o644)
	}
	got, err := Pool(dir)
	if err != nil || len(got) != 2 || filepath.Base(got[0]) != "1-indigo.png" {
		t.Fatalf("got %v %v", got, err)
	}
	if p, err := Pool(filepath.Join(dir, "nope")); err != nil || p != nil {
		t.Errorf("没有池子目录不是错误：%v %v", p, err)
	}
}
