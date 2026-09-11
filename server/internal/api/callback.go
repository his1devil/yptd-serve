package api

import (
	"context"
	"encoding/json"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/config"
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
	ClientMsgID    string   `json:"clientMsgID"`
	SendID         string   `json:"sendID"`
	RecvID         string   `json:"recvID"`
	GroupID        string   `json:"groupID"`
	SenderNickname string   `json:"senderNickname"`
	ContentType    int32    `json:"contentType"`
	Content        string   `json:"content"`
	AtUserList     []string `json:"atUserList"`
	Ex             string   `json:"ex"`
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

	text, inContent := bot.ParseContent(req.ContentType, req.Content, s.bot.Nicknames())
	addressed := addressedAgents(req, inContent, s.bot.Agents(), command)
	base := bot.Message{
		ClientMsgID:    req.ClientMsgID,
		SenderID:       req.SendID,
		SenderNickname: req.SenderNickname,
		GroupID:        req.GroupID,
		ConversationID: conversationID(req),
		ContentType:    req.ContentType,
		Text:           text,
		Attachments:    bot.ParseAttachments(req.Ex),
	}

	// One message may call on several agents, and each answers for itself.
	// They queue behind one another per conversation rather than talking at
	// the same time.
	var handling []string
	for _, agentID := range addressed {
		message := base
		message.AgentID = agentID
		if !s.bot.Wants(message) {
			continue
		}
		handling = append(handling, agentID)
		go func(m bot.Message) {
			// Its own context: the request's is cancelled the moment this
			// handler returns, and the agent takes far longer than that.
			ctx, cancel := context.WithTimeout(context.Background(), s.botBudget)
			defer cancel()
			s.bot.Handle(ctx, m)
		}(message)
	}

	// One line per addressed-looking message, because the alternative when
	// the bot stays silent is guessing which of five conditions rejected it.
	s.log.Info("callback",
		"command", command, "from", req.SendID, "group", req.GroupID,
		"recv", req.RecvID, "type", req.ContentType, "at", req.AtUserList,
		"addressed", addressed, "text", trimForLog(text),
		"handling", handling)
	callbackOK(w)
}

func callbackOK(w http.ResponseWriter) {
	writeJSON(w, http.StatusOK, map[string]any{"errCode": 0, "errMsg": "", "errDlt": ""})
}

// addressedAgents: in a group an agent must be mentioned; in a direct chat
// the message being sent to it is the whole of being addressed.
//
// Returned in roster order rather than mention order, so that when someone
// calls on two agents at once the group always hears them in the same
// sequence.
func addressedAgents(req callbackReq, inContent []string, agents []config.Agent, command string) []string {
	if command == afterSendSingleMsg || req.GroupID == "" {
		for _, a := range agents {
			if a.UserID == req.RecvID {
				return []string{a.UserID}
			}
		}
		return nil
	}
	// Two places carry the mentions and neither is always filled: the
	// callback's own field is what the SDK sends, and the copy inside the
	// message element is what the REST API sends. Reading only one means
	// silently ignoring half the ways a message can arrive.
	mentioned := make(map[string]bool, len(req.AtUserList)+len(inContent))
	for _, id := range req.AtUserList {
		mentioned[id] = true
	}
	for _, id := range inContent {
		mentioned[id] = true
	}
	var out []string
	for _, a := range agents {
		if mentioned[a.UserID] {
			out = append(out, a.UserID)
		}
	}
	return out
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
