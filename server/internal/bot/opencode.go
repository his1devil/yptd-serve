// Package bot answers messages addressed to the agent account.
//
// The bot is not an IM client. It never connects to OpenIM, holds no token
// and runs no SDK: OpenIM posts a webhook here when somebody speaks, and the
// reply goes back out through the admin API. That leaves nothing to keep
// alive, nothing to reconnect, and no messages lost when this process
// restarts.
package bot

import (
	"bytes"
	"context"
	"encoding/json"
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
	}
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
func (o *Opencode) Ask(ctx context.Context, sessionID, prompt string) (string, error) {
	in := map[string]any{
		"model": map[string]string{"providerID": o.provider, "modelID": o.model},
		"parts": []map[string]string{{"type": "text", "text": prompt}},
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
