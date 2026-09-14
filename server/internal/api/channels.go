package api

import (
	"net/http"
	"strings"

	"github.com/his1devil/yptd/server/internal/channel"
)

// handleChannels searches the channel directory.
//
// This has to be here rather than in the client: the SDK's own searchGroups
// only reads its local database, so it can only find rooms you have already
// joined — exactly the ones you do not need a directory for. OpenIM's search
// is on a management route that needs the admin token, so this service stands
// in front of it and applies the two things the raw search does not know
// about: which channels agreed to be listed, and which ones you are in.
//
// Channels that are findable but not open are still returned, marked
// `joinable: false`. Knowing a room exists and asking someone to invite you is
// useful; pretending it does not exist is not.
func (s *Server) handleChannels(w http.ResponseWriter, r *http.Request) {
	cred, ok := s.authed(w, r)
	if !ok {
		return
	}
	q := strings.TrimSpace(r.URL.Query().Get("q"))
	if q == "" {
		writeJSON(w, http.StatusOK, map[string]any{"channels": []any{}})
		return
	}
	found, err := s.openim.SearchGroups(r.Context(), q, 30)
	if err != nil {
		s.fail500(w, "search groups", err)
		return
	}
	joined, err := s.openim.JoinedGroups(r.Context(), cred.UserID)
	if err != nil {
		// 算不出「已经在哪些群」就不给结果，而不是把已经在的群也列出来：
		// 目录里出现一个你已经在的频道、还带个「加入」按钮，比搜不到更糟。
		s.fail500(w, "joined groups", err)
		return
	}
	mine := make(map[string]bool, len(joined))
	for _, id := range joined {
		mine[id] = true
	}

	out := make([]map[string]any, 0, len(found))
	for _, g := range found {
		if g.Dismissed() || mine[g.GroupID] {
			continue
		}
		p := channel.Parse(g.Ex)
		if !p.Findable {
			continue
		}
		out = append(out, map[string]any{
			"group_id": g.GroupID, "name": g.GroupName,
			"members": g.MemberCount, "joinable": p.Joinable,
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{"channels": out})
}
