// Package api is the HTTP surface the TUI talks to.
//
// Five endpoints, in the order a client meets them: register with an
// invitation, exchange a device token for an OpenIM session, the password
// fallback, a whoami, and the roster the invite picker lists. Everything
// else the TUI needs it gets from OpenIM directly with the token this
// service hands out.
package api

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"golang.org/x/crypto/bcrypt"

	"github.com/his1devil/yptd/server/internal/bot"
	"github.com/his1devil/yptd/server/internal/config"
	"github.com/his1devil/yptd/server/internal/invite"
	"github.com/his1devil/yptd/server/internal/openim"
	"github.com/his1devil/yptd/server/internal/store"
)

type Server struct {
	cfg    config.Config
	store  *store.Store
	openim *openim.Client
	log    *slog.Logger

	// bot is nil when no agent runtime is configured; the callback routes
	// then accept and discard, so OpenIM never sees an error.
	bot         *bot.Bot
	botUserID   string
	botNickname string
	botBudget   time.Duration
}

func New(cfg config.Config, st *store.Store, im *openim.Client, log *slog.Logger) *Server {
	s := &Server{cfg: cfg, store: st, openim: im, log: log}
	if cfg.BotEnabled() {
		agent := bot.NewOpencode(
			cfg.BotOpencodeURL, cfg.BotOpencodeUser, cfg.BotOpencodePassword,
			cfg.BotModel, cfg.BotTimeout,
		)
		s.bot = bot.New(bot.Config{
			UserID:        cfg.BotUserID,
			Nickname:      cfg.BotNickname,
			Timeout:       cfg.BotTimeout,
			MaxConcurrent: cfg.BotMaxConcurrent,
		}, im, st, agent, log)
		s.botUserID = cfg.BotUserID
		s.botNickname = cfg.BotNickname
		s.botBudget = botBudget(cfg.BotTimeout)
	}
	return s
}

func (s *Server) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/register", s.handleRegister)
	mux.HandleFunc("POST /v1/login", s.handleLogin)
	mux.HandleFunc("POST /v1/login/password", s.handleLoginPassword)
	mux.HandleFunc("GET /v1/me", s.handleMe)
	mux.HandleFunc("GET /v1/users", s.handleUsers)
	mux.HandleFunc("GET /healthz", s.handleHealth)
	// OpenIM appends the command name to the configured URL.
	mux.HandleFunc("POST /callback/", s.handleCallback)
	return s.withLogging(mux)
}

func (s *Server) withLogging(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rec := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(rec, r)
		s.log.Info("request",
			"method", r.Method, "path", r.URL.Path,
			"status", rec.status, "ms", time.Since(start).Milliseconds())
	})
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (r *statusRecorder) WriteHeader(code int) {
	r.status = code
	r.ResponseWriter.WriteHeader(code)
}

// ------------------------------------------------------------------ types ---

type registerReq struct {
	InviteCode string `json:"invite_code"`
	UserID     string `json:"user_id"`
	Nickname   string `json:"nickname"`
	DeviceName string `json:"device_name"`
	PlatformID int    `json:"platform_id"`
	// Password is optional: setting one enables the fallback login path.
	Password string `json:"password"`
}

type sessionResp struct {
	UserID string `json:"user_id"`
	// DeviceToken is returned only at registration. It is the long-lived
	// secret the client stores in its keychain.
	DeviceToken string `json:"device_token,omitempty"`
	IMToken     string `json:"im_token"`
	Nickname    string `json:"nickname"`
}

type loginReq struct {
	DeviceToken string `json:"device_token"`
	PlatformID  int    `json:"platform_id"`
}

type passwordLoginReq struct {
	UserID     string `json:"user_id"`
	Password   string `json:"password"`
	DeviceName string `json:"device_name"`
	PlatformID int    `json:"platform_id"`
}

// --------------------------------------------------------------- handlers ---

func (s *Server) handleRegister(w http.ResponseWriter, r *http.Request) {
	var req registerReq
	if !decode(w, r, &req) {
		return
	}

	code := invite.Normalize(req.InviteCode)
	if code == "" {
		fail(w, http.StatusBadRequest, "invalid_invite", "邀请码格式不对")
		return
	}
	userID := strings.TrimSpace(req.UserID)
	nickname := strings.TrimSpace(req.Nickname)
	if nickname == "" {
		fail(w, http.StatusBadRequest, "missing_nickname", "请填昵称")
		return
	}
	if userID == "" {
		userID = deriveUserID(nickname)
	}
	if !validUserID(userID) {
		fail(w, http.StatusBadRequest, "invalid_user_id", "用户名只能是 3-32 位字母、数字、下划线或连字符")
		return
	}

	ctx := r.Context()
	// Redeem first: an invitation consumed by a registration that then fails
	// is recoverable (issue another code); a code that survives a successful
	// registration is a second, unearned account.
	if err := s.store.RedeemInvite(ctx, code, userID); err != nil {
		switch {
		case errors.Is(err, store.ErrNotFound):
			fail(w, http.StatusBadRequest, "unknown_invite", "邀请码不存在")
		case errors.Is(err, store.ErrInviteUsed):
			fail(w, http.StatusBadRequest, "invite_used", "邀请码已被使用")
		case errors.Is(err, store.ErrInviteExpired):
			fail(w, http.StatusBadRequest, "invite_expired", "邀请码已过期")
		default:
			s.fail500(w, "redeem invite", err)
		}
		return
	}

	var passwordHash string
	if req.Password != "" {
		if len(req.Password) < 8 {
			fail(w, http.StatusBadRequest, "weak_password", "密码至少 8 位")
			return
		}
		h, err := bcrypt.GenerateFromPassword([]byte(req.Password), bcrypt.DefaultCost)
		if err != nil {
			s.fail500(w, "hash password", err)
			return
		}
		passwordHash = string(h)
	}

	if err := s.store.CreateUser(ctx, store.User{
		UserID:       userID,
		Nickname:     nickname,
		PasswordHash: passwordHash,
		InviteCode:   code,
	}); err != nil {
		if errors.Is(err, store.ErrUserExists) {
			fail(w, http.StatusConflict, "user_exists", "这个用户名已经有人用了")
			return
		}
		s.fail500(w, "create user", err)
		return
	}

	if err := s.openim.RegisterUser(ctx, userID, nickname, ""); err != nil {
		// OpenIM may already know this userID from an earlier partial run.
		// Registering again is the only failure we tolerate here.
		var apiErr *openim.Error
		if !errors.As(err, &apiErr) {
			s.fail500(w, "openim register", err)
			return
		}
		s.log.Warn("openim register returned an error, continuing",
			"user", userID, "code", apiErr.Code, "msg", apiErr.Msg)
	}

	s.issueSession(w, r, userID, nickname, req.DeviceName, req.PlatformID, true)
}

func (s *Server) handleLogin(w http.ResponseWriter, r *http.Request) {
	var req loginReq
	if !decode(w, r, &req) {
		return
	}
	if req.DeviceToken == "" {
		fail(w, http.StatusBadRequest, "missing_token", "缺少设备凭据")
		return
	}

	ctx := r.Context()
	cred, err := s.store.LookupCredential(ctx, req.DeviceToken)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			fail(w, http.StatusUnauthorized, "bad_token", "凭据无效或已被吊销，请用邀请码重新登录")
			return
		}
		s.fail500(w, "lookup credential", err)
		return
	}

	user, err := s.store.GetUser(ctx, cred.UserID)
	if err != nil {
		s.fail500(w, "get user", err)
		return
	}
	if user.Disabled {
		fail(w, http.StatusForbidden, "disabled", "账号已被停用")
		return
	}
	s.issueSession(w, r, user.UserID, user.Nickname, "", req.PlatformID, false)
}

func (s *Server) handleLoginPassword(w http.ResponseWriter, r *http.Request) {
	var req passwordLoginReq
	if !decode(w, r, &req) {
		return
	}
	ctx := r.Context()
	user, err := s.store.GetUser(ctx, strings.TrimSpace(req.UserID))
	if err != nil || user.PasswordHash == "" {
		// Same message and shape whether the account is missing or has no
		// password set, so this endpoint cannot be used to enumerate users.
		fail(w, http.StatusUnauthorized, "bad_credentials", "用户名或密码不对")
		return
	}
	if user.Disabled {
		fail(w, http.StatusForbidden, "disabled", "账号已被停用")
		return
	}
	if bcrypt.CompareHashAndPassword([]byte(user.PasswordHash), []byte(req.Password)) != nil {
		fail(w, http.StatusUnauthorized, "bad_credentials", "用户名或密码不对")
		return
	}
	s.issueSession(w, r, user.UserID, user.Nickname, req.DeviceName, req.PlatformID, true)
}

func (s *Server) handleMe(w http.ResponseWriter, r *http.Request) {
	token := bearer(r)
	if token == "" {
		fail(w, http.StatusUnauthorized, "missing_token", "缺少设备凭据")
		return
	}
	cred, err := s.store.LookupCredential(r.Context(), token)
	if err != nil {
		fail(w, http.StatusUnauthorized, "bad_token", "凭据无效")
		return
	}
	user, err := s.store.GetUser(r.Context(), cred.UserID)
	if err != nil {
		s.fail500(w, "get user", err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"user_id":  user.UserID,
		"nickname": user.Nickname,
		"disabled": user.Disabled,
	})
}

// handleUsers lists everyone on this server, for the invite picker. yptd is
// a small private server where everybody may see everybody; the roster is
// the right unit, and it needs a valid device credential like /v1/me.
func (s *Server) handleUsers(w http.ResponseWriter, r *http.Request) {
	token := bearer(r)
	if token == "" {
		fail(w, http.StatusUnauthorized, "missing_token", "缺少设备凭据")
		return
	}
	if _, err := s.store.LookupCredential(r.Context(), token); err != nil {
		fail(w, http.StatusUnauthorized, "bad_token", "凭据无效")
		return
	}
	users, err := s.store.ListUsers(r.Context())
	if err != nil {
		s.fail500(w, "list users", err)
		return
	}
	out := make([]map[string]any, 0, len(users))
	for _, u := range users {
		if u.Disabled {
			continue
		}
		out = append(out, map[string]any{"user_id": u.UserID, "nickname": u.Nickname})
	}
	writeJSON(w, http.StatusOK, map[string]any{"users": out})
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 8*time.Second)
	defer cancel()
	if err := s.openim.Ping(ctx); err != nil {
		writeJSON(w, http.StatusServiceUnavailable, map[string]any{"ok": false, "openim": err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// issueSession mints an OpenIM token and, when newDevice is set, a device
// credential to go with it.
func (s *Server) issueSession(w http.ResponseWriter, r *http.Request, userID, nickname, deviceName string, platformID int, newDevice bool) {
	if platformID == 0 {
		platformID = s.cfg.PlatformID
	}
	imToken, err := s.openim.UserToken(r.Context(), userID, platformID)
	if err != nil {
		s.fail500(w, "openim user token", err)
		return
	}

	resp := sessionResp{UserID: userID, IMToken: imToken, Nickname: nickname}
	if newDevice {
		plain, hash, err := store.NewToken()
		if err != nil {
			s.fail500(w, "new token", err)
			return
		}
		if err := s.store.CreateCredential(r.Context(), userID, hash, deviceName); err != nil {
			s.fail500(w, "create credential", err)
			return
		}
		resp.DeviceToken = plain
	}
	writeJSON(w, http.StatusOK, resp)
}

// ----------------------------------------------------------------- helpers ---

func decode(w http.ResponseWriter, r *http.Request, out any) bool {
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16))
	dec.DisallowUnknownFields()
	if err := dec.Decode(out); err != nil {
		fail(w, http.StatusBadRequest, "bad_request", "请求体解析失败: "+err.Error())
		return false
	}
	return true
}

func bearer(r *http.Request) string {
	h := r.Header.Get("Authorization")
	if after, ok := strings.CutPrefix(h, "Bearer "); ok {
		return strings.TrimSpace(after)
	}
	return ""
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}

func fail(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]string{"error": code, "message": message})
}

// fail500 keeps the cause in the log and out of the response: the client can
// act on "server error", not on a Mongo timeout string.
func (s *Server) fail500(w http.ResponseWriter, what string, err error) {
	s.log.Error("request failed", "at", what, "err", err)
	fail(w, http.StatusInternalServerError, "server_error", "服务端错误，请稍后再试")
}

func validUserID(id string) bool {
	if len(id) < 3 || len(id) > 32 {
		return false
	}
	for _, r := range id {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9', r == '_', r == '-':
		default:
			return false
		}
	}
	return true
}

// deriveUserID turns a nickname into a usable id, falling back to a random
// suffix when nothing survives the filter (an all-CJK nickname, for example).
func deriveUserID(nickname string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(nickname) {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '_', r == '-':
			b.WriteRune(r)
		case r == ' ':
			b.WriteByte('_')
		}
	}
	id := b.String()
	if len(id) < 3 {
		plain, _, err := store.NewToken()
		if err != nil {
			return ""
		}
		return "u_" + strings.ToLower(strings.TrimPrefix(plain, "yptd_"))[:10]
	}
	if len(id) > 32 {
		id = id[:32]
	}
	return id
}
