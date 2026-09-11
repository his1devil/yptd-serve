package api

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/his1devil/yptd/server/internal/invite"
	"github.com/his1devil/yptd/server/internal/store"
)

// What a desktop client needs beyond chat: renaming yourself, inviting a
// friend without asking the admin to SSH in, and knowing who the agents are.

// handleMePatch renames the caller. The roster clients read is this store,
// and OpenIM bakes the name into group member lists, so both get the change.
func (s *Server) handleMePatch(w http.ResponseWriter, r *http.Request) {
	cred, ok := s.authed(w, r)
	if !ok {
		return
	}
	var req struct {
		Nickname string `json:"nickname"`
	}
	if !decode(w, r, &req) {
		return
	}
	nickname := strings.TrimSpace(req.Nickname)
	if n := utf8.RuneCountInString(nickname); n == 0 || n > 32 {
		fail(w, http.StatusBadRequest, "bad_nickname", "昵称要 1–32 个字")
		return
	}
	if err := s.store.SetUserNickname(r.Context(), cred.UserID, nickname); err != nil {
		s.fail500(w, "set nickname", err)
		return
	}
	if err := s.openim.UpdateUser(r.Context(), cred.UserID, nickname); err != nil {
		// The roster already says the new name; OpenIM catching up later is
		// a cosmetic lag, not a failed rename.
		s.log.Warn("openim rename", "err", err, "user", cred.UserID)
	}
	writeJSON(w, http.StatusOK, map[string]any{"user_id": cred.UserID, "nickname": nickname})
}

// handleInviteNew mints invitation codes. Anyone with an account may: this
// is a friends-sized server where getting in already took an invitation, and
// the note records who vouched for whom.
func (s *Server) handleInviteNew(w http.ResponseWriter, r *http.Request) {
	cred, ok := s.authed(w, r)
	if !ok {
		return
	}
	var req struct {
		Note  string `json:"note"`
		Count int    `json:"count"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Count < 1 {
		req.Count = 1
	}
	if req.Count > 10 {
		fail(w, http.StatusBadRequest, "too_many", "一次最多 10 个")
		return
	}
	note := strings.TrimSpace(req.Note)
	if utf8.RuneCountInString(note) > 60 {
		fail(w, http.StatusBadRequest, "note_too_long", "备注最多 60 个字")
		return
	}
	note = strings.TrimSpace(cred.UserID + " 邀请 " + note)

	out := make([]map[string]any, 0, req.Count)
	for range req.Count {
		code, err := invite.New()
		if err != nil {
			s.fail500(w, "new invite", err)
			return
		}
		inv, err := s.store.CreateInvite(r.Context(), code, note, cred.UserID, s.cfg.InviteTTL)
		if err != nil {
			s.fail500(w, "create invite", err)
			return
		}
		out = append(out, map[string]any{"code": inv.Code, "expires_at": inv.ExpiresAt.UnixMilli(), "note": inv.Note})
	}
	writeJSON(w, http.StatusOK, map[string]any{"invites": out})
}

// handleInvites lists invitations. Unused ones by default; `?all=1` adds the
// redeemed and expired, so the admin page can show who came in on which code.
func (s *Server) handleInvites(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	invs, err := s.store.ListInvites(r.Context(), r.URL.Query().Get("all") == "1")
	if err != nil {
		s.fail500(w, "list invites", err)
		return
	}
	out := make([]map[string]any, 0, len(invs))
	for _, inv := range invs {
		row := map[string]any{
			"code": inv.Code, "note": inv.Note,
			"created_at": inv.CreatedAt.UnixMilli(), "expires_at": inv.ExpiresAt.UnixMilli(),
			"expired": time.Now().After(inv.ExpiresAt),
		}
		if inv.Used() {
			row["used_by"] = inv.UsedBy
			if inv.UsedAt != nil {
				row["used_at"] = inv.UsedAt.UnixMilli()
			}
		}
		out = append(out, row)
	}
	writeJSON(w, http.StatusOK, map[string]any{"invites": out})
}

// handleInviteCheck lets the sign-in form tell a mistyped code from a spent or
// stale one before it asks for a name. Unauthenticated by necessity; it
// answers nothing that register would not answer to the same caller. The code
// travels in the body, as it does for register, so it never lands in an
// access log.
func (s *Server) handleInviteCheck(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Code string `json:"code"`
	}
	if !decode(w, r, &req) {
		return
	}
	code := invite.Normalize(req.Code)
	if code == "" {
		writeJSON(w, http.StatusOK, map[string]any{"valid": false, "reason": "invalid"})
		return
	}
	inv, err := s.store.GetInvite(r.Context(), code)
	switch {
	case errors.Is(err, store.ErrNotFound):
		writeJSON(w, http.StatusOK, map[string]any{"valid": false, "reason": "unknown"})
		return
	case err != nil:
		s.fail500(w, "get invite", err)
		return
	case inv.Used():
		writeJSON(w, http.StatusOK, map[string]any{"valid": false, "reason": "used"})
		return
	case time.Now().After(inv.ExpiresAt):
		writeJSON(w, http.StatusOK, map[string]any{"valid": false, "reason": "expired"})
		return
	}
	out := map[string]any{"valid": true, "expires_at": inv.ExpiresAt.UnixMilli()}
	// The inviter's name makes the welcome personal; whoever holds the code
	// is the person it was handed to, so telling them who handed it over
	// gives nothing away.
	if inv.CreatedBy != "" {
		if u, err := s.store.GetUser(r.Context(), inv.CreatedBy); err == nil {
			out["invited_by"] = u.Nickname
		}
	}
	writeJSON(w, http.StatusOK, out)
}

// welcomeInviter tells whoever minted the code that their guest has arrived,
// speaking as the first agent. A newcomer's sidebar is empty until someone
// pulls them into a channel, and the inviter is the one who knows which.
func (s *Server) welcomeInviter(inv store.Invite, userID, nickname string) {
	if inv.CreatedBy == "" || inv.CreatedBy == userID || len(s.cfg.Agents) == 0 {
		return
	}
	a := s.cfg.Agents[0]
	text := fmt.Sprintf("%s（@%s）用你的邀请码进来了。去打个招呼，或者把 TA 拉进频道吧。", nickname, userID)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if _, err := s.openim.SendText(ctx, a.UserID, a.Nickname, inv.CreatedBy, "", text, ""); err != nil {
		s.log.Warn("welcome inviter", "err", err, "inviter", inv.CreatedBy, "user", userID)
	}
}

// handleAgents is the agent roster with what each one is for: identity from
// the configuration, the persona summary from opencode.
func (s *Server) handleAgents(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.authed(w, r); !ok {
		return
	}
	var descs map[string]string
	if s.bot != nil {
		descs = s.bot.Describe(r.Context())
	}
	out := make([]map[string]any, 0, len(s.cfg.Agents))
	for _, a := range s.cfg.Agents {
		tag := a.Tag
		if tag == "" {
			tag = "AGENT"
		}
		out = append(out, map[string]any{
			"user_id": a.UserID, "nickname": a.Nickname, "tag": tag, "color": a.Color,
			"model": a.Model, "opencode": a.Opencode, "description": descs[a.UserID],
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{"agents": out})
}
