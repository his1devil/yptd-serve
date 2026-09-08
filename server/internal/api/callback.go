package api

import (
	"context"
	"encoding/json"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/his1devil/yptd/server/internal/bot"
)

// OpenIM posts to <configured url>/<command>, so these paths are the command
// names verbatim rather than something tidier.
const (
	afterSendGroupMsg  = "callbackAfterSendGroupMsgCommand"
	afterSendSingleMsg = "callbackAfterSendSingleMsgCommand"
)

// callbackReq is the part of OpenIM's payload this service reads. The
// callback carries `atUserList` already parsed, which is what decides whether
// the bot was addressed -- no text matching needed.
type callbackReq struct {
	SendID         string   `json:"sendID"`
	RecvID         string   `json:"recvID"`
	GroupID        string   `json:"groupID"`
	SenderNickname string   `json:"senderNickname"`
	ContentType    int32    `json:"contentType"`
	Content        string   `json:"content"`
	AtUserList     []string `json:"atUserList"`
}

// handleCallback answers OpenIM's webhook.
//
// It must return within OpenIM's five-second callback timeout, so the work
// happens on its own goroutine and the response goes out immediately. A
// webhook is a notification, not a request for an answer.
func (s *Server) handleCallback(w http.ResponseWriter, r *http.Request) {
	command := strings.TrimPrefix(r.URL.Path, "/callback/")
	var req callbackReq
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&req); err != nil {
		s.log.Warn("callback: bad body", "command", command, "err", err)
		callbackOK(w)
		return
	}
	if s.bot == nil {
		callbackOK(w)
		return
	}

	text, _ := bot.ParseContent(req.ContentType, req.Content, s.botNickname)
	message := bot.Message{
		SenderID:       req.SendID,
		SenderNickname: req.SenderNickname,
		GroupID:        req.GroupID,
		ConversationID: conversationID(req),
		ContentType:    req.ContentType,
		Text:           text,
		Mentioned:      addressedToBot(req, s.botUserID, command),
	}
	// One line per addressed-looking message, because the alternative when
	// the bot stays silent is guessing which of five conditions rejected it.
	wanted := s.bot.Wants(message)
	s.log.Info("callback",
		"command", command, "from", req.SendID, "group", req.GroupID,
		"recv", req.RecvID, "type", req.ContentType, "at", req.AtUserList,
		"mentioned", message.Mentioned, "text", trimForLog(message.Text),
		"handling", wanted)
	if wanted {
		go func() {
			// Its own context: the request's is cancelled the moment this
			// handler returns, and the agent takes far longer than that.
			ctx, cancel := context.WithTimeout(context.Background(), s.botBudget)
			defer cancel()
			s.bot.Handle(ctx, message)
		}()
	}
	callbackOK(w)
}

func callbackOK(w http.ResponseWriter) {
	writeJSON(w, http.StatusOK, map[string]any{"errCode": 0, "errMsg": "", "errDlt": ""})
}

// addressedToBot: in a group the bot must be mentioned; in a direct chat the
// message being sent to it is the whole of being addressed.
func addressedToBot(req callbackReq, botUserID, command string) bool {
	if command == afterSendSingleMsg || req.GroupID == "" {
		return req.RecvID == botUserID
	}
	for _, id := range req.AtUserList {
		if id == botUserID {
			return true
		}
	}
	return false
}

// conversationID rebuilds the id OpenIM uses, which a revoke needs.
func conversationID(req callbackReq) string {
	if req.GroupID != "" {
		return "sg_" + req.GroupID
	}
	pair := []string{req.SendID, req.RecvID}
	sort.Strings(pair)
	return "si_" + pair[0] + "_" + pair[1]
}

func trimForLog(value string) string {
	if len(value) > 80 {
		return value[:80] + "…"
	}
	return value
}

// botBudget leaves room for the agent's own timeout plus the round trips
// around it.
func botBudget(agent time.Duration) time.Duration { return agent + time.Minute }
