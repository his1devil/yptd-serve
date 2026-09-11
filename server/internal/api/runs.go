package api

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"sort"
	"strconv"
	"time"

	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/run"
	"github.com/his1devil/yptd/server/internal/store"
)

// The run endpoints: what an agent is doing right now, streamed; what it did,
// on record; and a way to make it stop.

// authed checks the device credential and answers the request itself when it
// is missing or bad.
func (s *Server) authed(w http.ResponseWriter, r *http.Request) (store.Credential, bool) {
	token := bearer(r)
	if token == "" {
		fail(w, http.StatusUnauthorized, "missing_token", "缺少设备凭据")
		return store.Credential{}, false
	}
	cred, err := s.store.LookupCredential(r.Context(), token)
	if err != nil {
		fail(w, http.StatusUnauthorized, "bad_token", "凭据无效")
		return store.Credential{}, false
	}
	return cred, true
}

// handleRunEvents streams one run: a snapshot of everything so far, then each
// event as it happens, until the run ends. Server-sent events, because the
// traffic is one way and a plain HTTP response survives every proxy.
func (s *Server) handleRunEvents(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	if s.bot == nil {
		fail(w, http.StatusNotFound, "no_bot", "这个服务端没有配置 agent")
		return
	}
	id := r.PathValue("id")

	rc := http.NewResponseController(w)
	// The server's write timeout is sized for JSON replies; a stream lives as
	// long as the run does.
	_ = rc.SetWriteDeadline(time.Time{})
	h := w.Header()
	h.Set("Content-Type", "text/event-stream; charset=utf-8")
	h.Set("Cache-Control", "no-cache")
	h.Set("X-Accel-Buffering", "no") // nginx: pass each event through as it comes
	w.WriteHeader(http.StatusOK)

	send := func(event string, payload any) bool {
		raw, err := json.Marshal(payload)
		if err != nil {
			return false
		}
		if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, raw); err != nil {
			return false
		}
		return rc.Flush() == nil
	}

	live, ok := s.bot.Runs().Get(id)
	if !ok {
		// Long finished. The record has everything a viewer can still show.
		sum, err := s.store.GetRun(r.Context(), id)
		if err != nil {
			send("error", map[string]string{"message": "没有这个运行记录"})
			return
		}
		send("snapshot", sum)
		send("end", map[string]string{"status": sum.Status})
		return
	}

	snap, events, cancel := live.Subscribe()
	defer cancel()
	if !send("snapshot", snap) {
		return
	}
	// Comments keep the connection warm through nginx's read timeout when a
	// long tool call produces nothing for a while.
	heartbeat := time.NewTicker(15 * time.Second)
	defer heartbeat.Stop()
	for {
		select {
		case ev, open := <-events:
			if !open {
				send("end", map[string]string{"status": live.Snapshot().Status})
				return
			}
			if !send(ev.Type, ev) {
				return
			}
			if ev.Type == "done" {
				send("end", map[string]string{"status": live.Snapshot().Status})
				return
			}
		case <-heartbeat.C:
			if _, err := fmt.Fprint(w, ": ping\n\n"); err != nil || rc.Flush() != nil {
				return
			}
		case <-r.Context().Done():
			return
		}
	}
}

// handleRun is one run's current snapshot.
func (s *Server) handleRun(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	id := r.PathValue("id")
	if s.bot != nil {
		if live, ok := s.bot.Runs().Get(id); ok {
			writeJSON(w, http.StatusOK, live.Snapshot())
			return
		}
	}
	sum, err := s.store.GetRun(r.Context(), id)
	if errors.Is(err, store.ErrNotFound) {
		fail(w, http.StatusNotFound, "no_run", "没有这个运行记录")
		return
	}
	if err != nil {
		s.fail500(w, "get run", err)
		return
	}
	writeJSON(w, http.StatusOK, sum)
}

// handleRuns lists a conversation's runs, newest first: the ones still going
// from memory, the rest from the record.
func (s *Server) handleRuns(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	conversation := r.URL.Query().Get("conversation")
	if conversation == "" {
		fail(w, http.StatusBadRequest, "missing_conversation", "要带 conversation 参数")
		return
	}
	limit, _ := strconv.ParseInt(r.URL.Query().Get("limit"), 10, 64)
	stored, err := s.store.ListRuns(r.Context(), conversation, limit)
	if err != nil {
		s.fail500(w, "list runs", err)
		return
	}
	seen := make(map[string]bool, len(stored))
	out := make([]run.Summary, 0, len(stored)+2)
	if s.bot != nil {
		for _, live := range s.bot.Runs().Active() {
			snap := live.Snapshot()
			if snap.ConversationID == conversation {
				out = append(out, snap)
				seen[snap.ID] = true
			}
		}
	}
	for _, sum := range stored {
		if !seen[sum.ID] {
			out = append(out, sum)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].StartedAt > out[j].StartedAt })
	writeJSON(w, http.StatusOK, map[string]any{"runs": out})
}

// handleRunCancel stops a run. Anyone in the chat may: the run is answering
// the chat, and a wrong question should not have to finish.
func (s *Server) handleRunCancel(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	if s.bot == nil {
		fail(w, http.StatusNotFound, "no_bot", "这个服务端没有配置 agent")
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()
	if err := s.bot.Cancel(ctx, r.PathValue("id")); err != nil {
		if errors.Is(err, bot.ErrNoRun) {
			fail(w, http.StatusNotFound, "no_run", "这个运行已经结束或不存在")
			return
		}
		s.fail500(w, "cancel run", err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
