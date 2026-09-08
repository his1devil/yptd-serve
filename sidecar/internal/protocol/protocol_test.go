package protocol

import (
	"bytes"
	"encoding/json"
	"io"
	"strings"
	"sync"
	"testing"
)

func decodeLines(t *testing.T, raw string) []map[string]any {
	t.Helper()
	var out []map[string]any
	for _, line := range strings.Split(strings.TrimSpace(raw), "\n") {
		if line == "" {
			continue
		}
		var frame map[string]any
		if err := json.Unmarshal([]byte(line), &frame); err != nil {
			t.Fatalf("line %q is not JSON: %v", line, err)
		}
		out = append(out, frame)
	}
	return out
}

func TestRepliesCarryAnIDAndEventsNeverDo(t *testing.T) {
	var buf bytes.Buffer
	w := NewWriter(&buf)

	if err := w.Reply(7, json.RawMessage(`{"ok":true}`)); err != nil {
		t.Fatal(err)
	}
	if err := w.Emit("OnRecvNewMessage", json.RawMessage(`{"clientMsgID":"x"}`)); err != nil {
		t.Fatal(err)
	}
	if err := w.Fail(8, 1501, "token expired"); err != nil {
		t.Fatal(err)
	}

	frames := decodeLines(t, buf.String())
	if len(frames) != 3 {
		t.Fatalf("expected 3 frames, got %d", len(frames))
	}

	// This is the whole disambiguation rule the client relies on.
	if _, ok := frames[0]["id"]; !ok {
		t.Error("a reply must carry an id")
	}
	if _, ok := frames[1]["id"]; ok {
		t.Error("an event must not carry an id")
	}
	if frames[1]["ev"] != "OnRecvNewMessage" {
		t.Errorf("event name = %v", frames[1]["ev"])
	}
	if frames[2]["ok"] != false || frames[2]["msg"] != "token expired" {
		t.Errorf("failure frame = %v", frames[2])
	}
}

func TestEveryFrameIsExactlyOneLine(t *testing.T) {
	var buf bytes.Buffer
	w := NewWriter(&buf)
	// A payload containing a newline must not split the frame.
	_ = w.EmitValue("OnRecvNewMessage", map[string]string{"text": "第一行\n第二行"})
	if got := strings.Count(buf.String(), "\n"); got != 1 {
		t.Fatalf("frame spans %d newlines, want 1: %q", got, buf.String())
	}
}

func TestConcurrentWritesDoNotInterleave(t *testing.T) {
	var buf bytes.Buffer
	w := NewWriter(&buf)

	// The SDK delivers callbacks from its own goroutines; two frames written
	// at once must not merge into one unparseable line.
	var wg sync.WaitGroup
	for i := range 50 {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_ = w.EmitValue("Tick", map[string]int{"n": i})
		}(i)
	}
	wg.Wait()

	frames := decodeLines(t, buf.String())
	if len(frames) != 50 {
		t.Fatalf("expected 50 frames, got %d", len(frames))
	}
	seen := map[float64]bool{}
	for _, f := range frames {
		data, ok := f["data"].(map[string]any)
		if !ok {
			t.Fatalf("frame lost its payload: %v", f)
		}
		seen[data["n"].(float64)] = true
	}
	if len(seen) != 50 {
		t.Errorf("saw %d distinct values, want 50", len(seen))
	}
}

func TestReaderYieldsRequestsAndThenEOF(t *testing.T) {
	input := `{"id":1,"op":"groups"}
{"id":2,"op":"send","args":{"group_id":"g1"}}

`
	r := NewReader(strings.NewReader(input))

	first, err := r.Next()
	if err != nil {
		t.Fatal(err)
	}
	if first.ID != 1 || first.Op != "groups" {
		t.Errorf("first = %+v", first)
	}

	second, err := r.Next()
	if err != nil {
		t.Fatal(err)
	}
	if second.Op != "send" || !json.Valid(second.Args) {
		t.Errorf("second = %+v", second)
	}

	if _, err := r.Next(); err != io.EOF {
		t.Errorf("expected EOF, got %v", err)
	}
}

func TestAMalformedLineIsReportedNotFatal(t *testing.T) {
	r := NewReader(strings.NewReader("not json\n{\"id\":9,\"op\":\"self\"}\n"))

	if _, err := r.Next(); err == nil {
		t.Fatal("expected a decode error")
	}
	// The connection survives: the next well-formed line still parses.
	req, err := r.Next()
	if err != nil {
		t.Fatalf("reader did not recover: %v", err)
	}
	if req.ID != 9 || req.Op != "self" {
		t.Errorf("recovered request = %+v", req)
	}
}

func TestALargeFrameIsNotTruncated(t *testing.T) {
	// A merged forward or an image caption can exceed the default 64 KB
	// scanner token; truncation would surface as a confusing parse error.
	big := strings.Repeat("漢", 200_000) // ~600 KB of UTF-8
	payload, err := json.Marshal(Request{ID: 1, Op: "send", Args: mustJSON(big)})
	if err != nil {
		t.Fatal(err)
	}
	r := NewReader(bytes.NewReader(append(payload, '\n')))
	req, err := r.Next()
	if err != nil {
		t.Fatalf("large frame rejected: %v", err)
	}
	var text string
	if err := json.Unmarshal(req.Args, &text); err != nil {
		t.Fatal(err)
	}
	if len([]rune(text)) != 200_000 {
		t.Errorf("payload truncated to %d runes", len([]rune(text)))
	}
}

func mustJSON(v any) json.RawMessage {
	out, err := json.Marshal(v)
	if err != nil {
		panic(err)
	}
	return out
}
