// Package bot answers messages addressed to the agent account.
//
// The bot is not an IM client. It never connects to OpenIM, holds no token
// and runs no SDK: OpenIM posts a webhook here when somebody speaks, and the
// reply goes back out through the admin API. That leaves nothing to keep
// alive, nothing to reconnect, and no messages lost when this process
// restarts.
package bot

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// Opencode talks to a headless `opencode serve`.
//
// Its own HTTP API rather than the `opencode run` CLI, because a session has
// to survive between questions: asking a follow-up in the same group should
// continue the same conversation, and a fresh process every time cannot.
type Opencode struct {
	base     string
	user     string
	password string
	provider string
	model    string
	http     *http.Client
	// stream has no timeout: it holds the event subscription open for the
	// life of the process, and a client timeout would cut it every few minutes.
	stream *http.Client
}

func NewOpencode(base, user, password, model string, timeout time.Duration) *Opencode {
	provider, id, ok := strings.Cut(model, "/")
	if !ok {
		provider, id = "zhipuai", model
	}
	return &Opencode{
		base:     strings.TrimRight(base, "/"),
		user:     user,
		password: password,
		provider: provider,
		model:    id,
		http:     &http.Client{Timeout: timeout},
		stream:   &http.Client{},
	}
}

func (o *Opencode) resolve(model string) (provider, id string) {
	provider, id = o.provider, o.model
	if model != "" {
		if p, m, ok := strings.Cut(model, "/"); ok {
			provider, id = p, m
		} else {
			id = model
		}
	}
	return provider, id
}

func (o *Opencode) do(ctx context.Context, method, path string, in, out any) error {
	var body io.Reader
	if in != nil {
		raw, err := json.Marshal(in)
		if err != nil {
			return err
		}
		body = bytes.NewReader(raw)
	}
	req, err := http.NewRequestWithContext(ctx, method, o.base+path, body)
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	if o.user != "" {
		req.SetBasicAuth(o.user, o.password)
	}
	resp, err := o.http.Do(req)
	if err != nil {
		return fmt.Errorf("opencode %s: %w", path, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		detail, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
		return fmt.Errorf("opencode %s: HTTP %d: %s", path, resp.StatusCode, strings.TrimSpace(string(detail)))
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// Health reports the running version, for the self-check.
func (o *Opencode) Health(ctx context.Context) (string, error) {
	var out struct {
		Healthy bool   `json:"healthy"`
		Version string `json:"version"`
	}
	if err := o.do(ctx, http.MethodGet, "/global/health", nil, &out); err != nil {
		return "", err
	}
	if !out.Healthy {
		return out.Version, fmt.Errorf("opencode reports unhealthy")
	}
	return out.Version, nil
}

// AgentNames lists the agents opencode knows about.
//
// For the self-check: a roster naming an agent opencode has never heard of
// does not fail, it quietly answers as the default one, and nothing in the
// chat log says so.
func (o *Opencode) AgentNames(ctx context.Context) ([]string, error) {
	var out []struct {
		Name string `json:"name"`
	}
	if err := o.do(ctx, http.MethodGet, "/agent", nil, &out); err != nil {
		return nil, err
	}
	names := make([]string, 0, len(out))
	for _, a := range out {
		names = append(names, a.Name)
	}
	return names, nil
}

// NewSession opens a conversation on the opencode side and returns its id.
func (o *Opencode) NewSession(ctx context.Context, title string) (string, error) {
	var out struct {
		ID string `json:"id"`
	}
	if err := o.do(ctx, http.MethodPost, "/session", map[string]any{"title": title}, &out); err != nil {
		return "", err
	}
	if out.ID == "" {
		return "", fmt.Errorf("opencode /session: empty id")
	}
	return out.ID, nil
}

// part is one piece of a reply: `step-start`, `reasoning`, `text`,
// `step-finish`, and tool calls in between.
type part struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// Ask sends one turn and waits for the whole answer.
//
// Synchronous on purpose: nobody is watching a terminal, and a chat message
// is a batch by nature. The caller is already off the webhook's thread.
//
// `agent` picks which of opencode's agents answers — that is where a persona
// and its tool scope live — and `model` overrides the default for this turn.
// Both are empty for a deployment with one assistant and one model.
func (o *Opencode) Ask(ctx context.Context, sessionID, agent, model, prompt string) (string, error) {
	provider, id := o.provider, o.model
	if model != "" {
		if p, m, ok := strings.Cut(model, "/"); ok {
			provider, id = p, m
		} else {
			id = model
		}
	}
	in := map[string]any{
		"model": map[string]string{"providerID": provider, "modelID": id},
		"parts": []map[string]string{{"type": "text", "text": prompt}},
	}
	if agent != "" {
		in["agent"] = agent
	}
	var out struct {
		Parts []part `json:"parts"`
	}
	if err := o.do(ctx, http.MethodPost, "/session/"+sessionID+"/message", in, &out); err != nil {
		return "", err
	}
	return answerText(out.Parts), nil
}

// answerText keeps what the model said and drops how it got there.
//
// GLM is a reasoning model, and its `reasoning` part is routinely longer
// than the answer; posting that to a group chat would bury the answer in
// thinking nobody asked for.
func answerText(parts []part) string {
	var said []string
	for _, p := range parts {
		if p.Type != "text" {
			continue
		}
		if text := strings.TrimSpace(p.Text); text != "" {
			said = append(said, text)
		}
	}
	return strings.Join(said, "\n\n")
}

// ------------------------------------------------------------- streaming ---

// Event is one line of opencode's event stream, kept raw until somebody
// needs the inside.
type Event struct {
	Type       string          `json:"type"`
	Properties json.RawMessage `json:"properties"`
}

// PromptAsync sends one turn and returns as soon as opencode has accepted it.
// The answer arrives through Events; see stream.go for how it is read.
func (o *Opencode) PromptAsync(ctx context.Context, sessionID, agent, model, prompt string) error {
	provider, id := o.resolve(model)
	in := map[string]any{
		"model": map[string]string{"providerID": provider, "modelID": id},
		"parts": []map[string]string{{"type": "text", "text": prompt}},
	}
	if agent != "" {
		in["agent"] = agent
	}
	return o.do(ctx, http.MethodPost, "/session/"+sessionID+"/prompt_async", in, nil)
}

// Abort stops whatever the session is doing.
func (o *Opencode) Abort(ctx context.Context, sessionID string) error {
	return o.do(ctx, http.MethodPost, "/session/"+sessionID+"/abort", nil, nil)
}

// Busy reports whether the session is still working. A session missing from
// the status map is idle.
func (o *Opencode) Busy(ctx context.Context, sessionID string) (bool, error) {
	var out map[string]struct {
		Type string `json:"type"`
	}
	if err := o.do(ctx, http.MethodGet, "/session/status", nil, &out); err != nil {
		return false, err
	}
	st, ok := out[sessionID]
	return ok && st.Type != "idle", nil
}

// LastAnswerSince is the text of the newest assistant message created after
// `since` (unix ms), for when the stream lost the answer on the way.
func (o *Opencode) LastAnswerSince(ctx context.Context, sessionID string, since int64) (string, error) {
	var out []struct {
		Info struct {
			Role string `json:"role"`
			Time struct {
				Created int64 `json:"created"`
			} `json:"time"`
		} `json:"info"`
		Parts []part `json:"parts"`
	}
	if err := o.do(ctx, http.MethodGet, "/session/"+sessionID+"/message", nil, &out); err != nil {
		return "", err
	}
	for i := len(out) - 1; i >= 0; i-- {
		m := out[i]
		if m.Info.Role != "assistant" || m.Info.Time.Created < since-2000 {
			continue
		}
		if text := answerText(m.Parts); text != "" {
			return text, nil
		}
	}
	return "", nil
}

// Events subscribes to opencode's event stream and calls handle for each
// event until the connection drops or ctx ends. It always returns an error,
// because the stream is not supposed to end.
func (o *Opencode) Events(ctx context.Context, handle func(Event)) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, o.base+"/event", nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "text/event-stream")
	if o.user != "" {
		req.SetBasicAuth(o.user, o.password)
	}
	resp, err := o.stream.Do(req)
	if err != nil {
		return fmt.Errorf("opencode /event: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("opencode /event: HTTP %d", resp.StatusCode)
	}

	sc := bufio.NewScanner(resp.Body)
	// A tool's output rides inside one event; a screenful of `cat` is normal.
	sc.Buffer(make([]byte, 0, 64<<10), 8<<20)
	var data strings.Builder
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			if data.Len() > 0 {
				var ev Event
				if json.Unmarshal([]byte(data.String()), &ev) == nil && ev.Type != "" {
					handle(ev)
				}
				data.Reset()
			}
			continue
		}
		if rest, ok := strings.CutPrefix(line, "data:"); ok {
			data.WriteString(strings.TrimPrefix(rest, " "))
		}
	}
	if err := sc.Err(); err != nil {
		return fmt.Errorf("opencode /event: %w", err)
	}
	return errors.New("opencode /event: stream ended")
}
