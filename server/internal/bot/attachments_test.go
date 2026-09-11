package bot

import "testing"

func TestParseAttachmentsReadsYptdRichExAndIgnoresTheRest(t *testing.T) {
	ex := `{"yptd":"rich","a":[{"k":"i","u":"https://x/a.png","n":"a.png","s":10,"w":4,"h":3},{"k":"f","u":"https://x/b.pdf","n":"b.pdf","s":20},{"k":"i","u":"","n":"lost"}],"t":0}`
	got := ParseAttachments(ex)
	if len(got) != 2 {
		t.Fatalf("want 2 attachments, got %d: %+v", len(got), got)
	}
	if got[0].Kind != "image" || got[0].URL != "https://x/a.png" || got[0].Name != "a.png" {
		t.Errorf("first: %+v", got[0])
	}
	if got[1].Kind != "file" || got[1].Name != "b.pdf" {
		t.Errorf("second: %+v", got[1])
	}
	for _, other := range []string{"", "{", `{"yptd":"run","run":"r1"}`, `{"yptd":"pending"}`} {
		if ParseAttachments(other) != nil {
			t.Errorf("%q should carry no attachments", other)
		}
	}
}
