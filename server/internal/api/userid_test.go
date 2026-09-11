package api

import "testing"

// OpenIM's /user/user_register answers 1001 ArgsError for any userID holding a
// hyphen, wherever it sits. We learn that here rather than in production,
// where the invitation has already been spent by the time the call is made.
func TestValidUserIDRejectsHyphen(t *testing.T) {
	for _, id := range []string{"app-", "-app", "li-ming", "a-b-c"} {
		if validUserID(id) {
			t.Errorf("validUserID(%q) = true, want false: OpenIM rejects hyphens", id)
		}
	}
	for _, id := range []string{"app", "li_ming", "u_0a1b2c3d4e", "Zhang3"} {
		if !validUserID(id) {
			t.Errorf("validUserID(%q) = false, want true", id)
		}
	}
}

func TestDeriveUserIDNeverProducesSomethingOpenIMRefuses(t *testing.T) {
	nicknames := []string{
		"app-联调",  // the case that broke: ASCII + CJK, separator left dangling
		"张三",      // no ASCII at all -- falls back to a generated id
		"Li-Ming", // a hyphen a person would actually type
		"Zhang San",
		"-",
		"a-b",
		"  ",
		"用户-2026",
	}
	for _, nick := range nicknames {
		id := deriveUserID(nick)
		if id == "" {
			t.Errorf("deriveUserID(%q) = %q, want a usable id", nick, id)
			continue
		}
		if !validUserID(id) {
			t.Errorf("deriveUserID(%q) = %q, which validUserID rejects", nick, id)
		}
	}
}

func TestDeriveUserIDKeepsWordBoundaries(t *testing.T) {
	for nick, want := range map[string]string{
		"Zhang San": "zhang_san",
		"Li-Ming":   "li_ming",
		"app-联调":    "app",
		"用户-2026":   "2026",
	} {
		if got := deriveUserID(nick); got != want {
			t.Errorf("deriveUserID(%q) = %q, want %q", nick, got, want)
		}
	}
}
