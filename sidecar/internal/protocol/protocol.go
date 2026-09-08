// Package protocol is the wire format between the TUI and the sidecar.
//
// NDJSON over a Unix socket, in two directions that never interleave
// ambiguously: a reply always carries the `id` of the request it answers, and
// an event never carries one. That single rule is why the client needs no
// state machine to tell them apart.
package protocol

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"sync"
)

// Request is one call from the TUI.
type Request struct {
	ID   uint64          `json:"id"`
	Op   string          `json:"op"`
	Args json.RawMessage `json:"args,omitempty"`
}

// Response answers exactly one Request.
type Response struct {
	ID   uint64          `json:"id"`
	OK   bool            `json:"ok"`
	Data json.RawMessage `json:"data,omitempty"`
	Code int32           `json:"code,omitempty"`
	Msg  string          `json:"msg,omitempty"`
}

// Event is unsolicited: an incoming message, a connection change, a sync tick.
type Event struct {
	Ev   string          `json:"ev"`
	Data json.RawMessage `json:"data,omitempty"`
}

// Writer serializes frames onto the socket.
//
// The SDK delivers callbacks from its own goroutines, so several events can be
// produced at once; a mutex here is what keeps two frames from interleaving
// into one unparseable line.
type Writer struct {
	mu  sync.Mutex
	out *bufio.Writer
}

func NewWriter(w io.Writer) *Writer {
	return &Writer{out: bufio.NewWriterSize(w, 64*1024)}
}

func (w *Writer) write(value any) error {
	payload, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("protocol: encode: %w", err)
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if _, err := w.out.Write(payload); err != nil {
		return err
	}
	if err := w.out.WriteByte('\n'); err != nil {
		return err
	}
	// Flushed per frame: a buffered event that arrives "soon" is a message the
	// user does not see yet.
	return w.out.Flush()
}

func (w *Writer) Reply(id uint64, data json.RawMessage) error {
	return w.write(Response{ID: id, OK: true, Data: data})
}

func (w *Writer) Fail(id uint64, code int32, msg string) error {
	return w.write(Response{ID: id, OK: false, Code: code, Msg: msg})
}

func (w *Writer) Emit(name string, data json.RawMessage) error {
	return w.write(Event{Ev: name, Data: data})
}

// EmitValue marshals `value` and emits it. A payload that fails to encode is
// reported as an event of its own rather than dropped, so a bug here shows up
// in the client instead of as silence.
func (w *Writer) EmitValue(name string, value any) error {
	payload, err := json.Marshal(value)
	if err != nil {
		return w.Emit("SidecarError", json.RawMessage(
			fmt.Sprintf(`{"at":%q,"err":%q}`, name, err.Error())))
	}
	return w.Emit(name, payload)
}

// Reader decodes requests from the socket.
type Reader struct {
	scanner *bufio.Scanner
}

func NewReader(r io.Reader) *Reader {
	scanner := bufio.NewScanner(r)
	// A message with a large image or a merged forward can be big; the default
	// 64 KB token limit would truncate it into a parse error.
	scanner.Buffer(make([]byte, 0, 64*1024), 8*1024*1024)
	return &Reader{scanner: scanner}
}

// Next returns the next request, io.EOF at end of stream, or a decode error
// for a malformed line. A decode error is not fatal to the connection: the
// caller reports it and reads on.
func (r *Reader) Next() (Request, error) {
	for r.scanner.Scan() {
		line := r.scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var req Request
		if err := json.Unmarshal(line, &req); err != nil {
			return Request{}, fmt.Errorf("protocol: decode: %w", err)
		}
		return req, nil
	}
	if err := r.scanner.Err(); err != nil {
		return Request{}, err
	}
	return Request{}, io.EOF
}
