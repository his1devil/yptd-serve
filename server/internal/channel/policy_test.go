package channel

import "testing"

func TestUnsetIsClosed(t *testing.T) {
	// 这个功能之前建的群，ex 都是空的。它们必须保持私密，
	// 而不是因为上线了就突然变成谁都搜得到、谁都进得来。
	for _, ex := range []string{"", "not json", `{"yptd":"rich","a":[]}`, `{"find":1,"join":1}`} {
		if got := Parse(ex); got.Findable || got.Joinable {
			t.Fatalf("%q 应该读成全关，得到 %+v", ex, got)
		}
	}
}

func TestRoundTrip(t *testing.T) {
	for _, p := range []Policy{{}, {Findable: true}, {Joinable: true}, {Findable: true, Joinable: true}} {
		if got := Parse(Encode(p)); got != p {
			t.Fatalf("%+v 存完再读变成 %+v", p, got)
		}
	}
}

func TestEncodeKeepsTheTag(t *testing.T) {
	// 别的东西将来也可能想在 ex 里占一角，靠这个 tag 区分
	if got := Encode(Policy{Findable: true}); got != `{"yptd":"channel","find":1,"join":0}` {
		t.Fatalf("存出来是 %s", got)
	}
}

func TestVerificationTracksJoinable(t *testing.T) {
	// 可加入却留在 AllNeedVerification 上，每一次被允许的加入都会变成
	// 一条没人会去看的挂起申请
	if got := Verification(Policy{Joinable: true}); got != Directly {
		t.Fatalf("可加入的频道要设成直接进，得到 %d", got)
	}
	if got := Verification(Policy{}); got != AllNeedVerification {
		t.Fatalf("不开放的频道要设成需验证，得到 %d", got)
	}
}
