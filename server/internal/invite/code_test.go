package invite

import (
	"strings"
	"testing"
)

func TestNewProducesReadableCodes(t *testing.T) {
	seen := map[string]bool{}
	for range 200 {
		code, err := New()
		if err != nil {
			t.Fatalf("New: %v", err)
		}
		if !strings.HasPrefix(code, Prefix+"-") {
			t.Fatalf("missing prefix: %q", code)
		}
		if got, want := len(code), len(Prefix)+groups*(groupLen+1); got != want {
			t.Fatalf("length %d, want %d: %q", got, want, code)
		}
		// The whole point of the alphabet is that these never appear.
		for _, bad := range []string{"I", "L", "O", "U"} {
			if strings.Contains(strings.TrimPrefix(code, Prefix), bad) {
				t.Fatalf("ambiguous character %s in %q", bad, code)
			}
		}
		if seen[code] {
			t.Fatalf("duplicate code within 200 draws: %q", code)
		}
		seen[code] = true
	}
}

func TestNormalizeAcceptsHowPeopleActuallyType(t *testing.T) {
	canonical := "YPTD-7K2M-9XQP"
	for _, input := range []string{
		canonical,
		"yptd-7k2m-9xqp",
		"YPTD7K2M9XQP",
		"  yptd 7k2m 9xqp  ",
		"7K2M-9XQP", // prefix omitted
		"7k2m9xqp",
	} {
		if got := Normalize(input); got != canonical {
			t.Errorf("Normalize(%q) = %q, want %q", input, got, canonical)
		}
	}
}

func TestNormalizeFoldsCrockfordAliases(t *testing.T) {
	// A person reading "0" aloud says "oh"; someone typing "1" may hit "l".
	if got, want := Normalize("YPTD-OK2M-9XQI"), "YPTD-0K2M-9XQ1"; got != want {
		t.Errorf("Normalize = %q, want %q", got, want)
	}
}

func TestNormalizeRejectsJunk(t *testing.T) {
	for _, input := range []string{
		"",
		"YPTD-7K2M",            // too short
		"YPTD-7K2M-9XQP-EXTRA", // too long
		"YPTD-7K2M-9XQ!",       // illegal character
		"hello world",
	} {
		if got := Normalize(input); got != "" {
			t.Errorf("Normalize(%q) = %q, want rejection", input, got)
		}
	}
}

func TestValidMatchesNormalize(t *testing.T) {
	code, err := New()
	if err != nil {
		t.Fatal(err)
	}
	if !Valid(code) {
		t.Errorf("freshly generated code rejected: %q", code)
	}
	if Valid("nope") {
		t.Error("junk accepted")
	}
}
