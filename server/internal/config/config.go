// Package config loads yptd-server settings from the environment.
//
// Environment rather than a config file, because almost every value here is
// either a secret or a deployment address: both belong in the unit file or
// the shell that starts the process, not in a file that is easy to commit by
// accident. The one exception is the agent roster, which is a list of records
// and does not fit a variable; the environment names the file instead.
package config

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"
)

type Config struct {
	// Listen is the HTTP bind address. Loopback by default: nginx terminates
	// TLS and proxies in, so the service never needs a public socket.
	Listen string

	MongoURI string
	MongoDB  string

	// OpenIMAPI is the base URL of openim-api.
	OpenIMAPI string
	// PublicAPI is the same service as clients reach it (https://im.example.com).
	// Only needed to upload files: OpenIM builds object URLs from the request's
	// host, and OpenIMAPI is loopback.
	PublicAPI string
	// AgentArtDir holds the portraits agents wear when agents.json names none:
	// one image per file, picked by a stable hash of the agent's id.
	AgentArtDir string
	// OpenIMSecret is the shared secret from OpenIM's share.yml. Holding it is
	// what lets this service mint tokens for anyone, which is exactly why the
	// TUI must not have it.
	OpenIMSecret string
	// OpenIMAdminUserID must match one of share.yml's imAdminUserID entries.
	OpenIMAdminUserID string

	// InviteTTL is how long a freshly minted invitation code stays usable.
	InviteTTL time.Duration
	// PlatformID sent to OpenIM when minting a user token.
	PlatformID int

	// Agents are the bot accounts. Messages from any of them are ignored,
	// and a message mentioning one is answered as that one.
	Agents []Agent
	// BotOpencodeURL is empty when no agent runtime is configured, which
	// disables the whole feature.
	BotOpencodeURL      string
	BotOpencodeUser     string
	BotOpencodePassword string
	BotModel            string
	BotTimeout          time.Duration
	BotMaxConcurrent    int
	// QuoteCLI is the `longbridge` executable the market watch shells out to.
	// Empty falls back to PATH; set it when the service runs as a user whose
	// PATH does not carry it. The CLI holds its own Longbridge credentials in
	// that user's home — this service never sees them.
	QuoteCLI string
	// QuoteHome is the HOME the quote CLI reads its Longbridge login from.
	// Needed because the CLI is logged in as the opencode user while this
	// service runs as another.
	QuoteHome string
	// NewsBase overrides the news feed's origin. Empty uses the real one;
	// this exists so a test or a staging box can point somewhere else.
	NewsBase string
}

// Agent is one bot account: an OpenIM identity, plus how to answer as it.
//
// Several may share the opencode server and even the model; what makes them
// different accounts is that each is @-able by name and keeps its own thread
// in every conversation.
type Agent struct {
	UserID   string `json:"user_id"`
	Nickname string `json:"nickname"`
	// Opencode names the agent `opencode` routes this account's turns to,
	// which is where the persona and the tool scope live. Empty leaves the
	// choice to opencode.
	Opencode string `json:"opencode"`
	// Model overrides the default for this account, as `provider/id`.
	Model string `json:"model"`
	// Tag is the short label a client draws beside the name. Defaults to
	// AGENT.
	Tag string `json:"tag"`
	// Color is this agent's identity colour, as a hex string. Assigned by
	// roster position when the file does not say, so several agents never
	// collide the way hashing their ids would.
	Color string `json:"color"`
	// Avatar 是头像的公网 URL，setup 时写进 OpenIM 的 faceURL。留空的 agent
	// 由客户端画 yptd 的标记。客户端认不认 faceURL 是客户端的事——桌面端目前
	// 对 agent 一律画标记，所以填了也可能看不到。
	Avatar string `json:"avatar"`
	// Watch turns on unprompted market pushes for this agent. Zero value is
	// off, which is what every agent but the quotes one wants.
	Watch Watch `json:"watch"`
	// News turns on unprompted news pushes for this agent. Zero value is off.
	News News `json:"news"`
}

// Watch is one agent's standing interest in a set of symbols: poll them, and
// when one moves past Threshold, say so in every group the agent belongs to.
//
// Deliberately agent-level rather than per-group. An agent is in a group
// because someone wanted its subject there, so its whole watchlist is in
// scope; per-group lists would need an editor nobody has asked for yet.
type Watch struct {
	// Symbols in Longbridge's <CODE>.<MARKET> form, e.g. 700.HK NVDA.US.
	// Empty disables the watch however the other fields are set.
	Symbols []string `json:"symbols" bson:"symbols,omitempty"`
	// Threshold is the move, in percent away from the previous close, that
	// makes a symbol worth interrupting a room over. Defaults to 3.
	Threshold float64 `json:"threshold" bson:"threshold,omitempty"`
	// Every is the poll interval as a Go duration, e.g. "2m". Defaults to
	// two minutes; anything under thirty seconds is raised to it, since the
	// quote CLI is a process spawn and the data is not tick-by-tick anyway.
	Every string `json:"every" bson:"every,omitempty"`
	// Groups limits which rooms hear the watch. Empty means every group the
	// agent belongs to, which is the steady state; a list is how a new
	// watchlist gets tried in one room before it starts interrupting all of
	// them.
	Groups []string `json:"groups" bson:"groups,omitempty"`
}

// Rooms narrows the agent's joined groups to the ones this watch may post in.
func (w Watch) Rooms(joined []string) []string { return rooms(w.Groups, joined) }

// rooms narrows a joined-group list to an allowlist. An empty allowlist means
// everywhere — that is the steady state, and a list is how a new watchlist
// gets tried in one room before it starts interrupting all of them.
func rooms(allow, joined []string) []string {
	if len(allow) == 0 {
		return joined
	}
	ok := make(map[string]bool, len(allow))
	for _, g := range allow {
		ok[g] = true
	}
	out := make([]string, 0, len(joined))
	for _, g := range joined {
		if ok[g] {
			out = append(out, g)
		}
	}
	return out
}

// On reports whether this watch should run at all.
func (w Watch) On() bool { return len(w.Symbols) > 0 }

// Tuned returns the watch with defaults and floors applied.
func (w Watch) Tuned() Watch {
	if w.Threshold <= 0 {
		w.Threshold = 3
	}
	d, err := time.ParseDuration(w.Every)
	switch {
	case err != nil || d <= 0:
		d = 2 * time.Minute
	case d < 30*time.Second:
		d = 30 * time.Second
	}
	w.Every = d.String()
	return w
}

// Interval is Every already parsed; Tuned guarantees it parses.
func (w Watch) Interval() time.Duration {
	d, err := time.ParseDuration(w.Tuned().Every)
	if err != nil {
		return 2 * time.Minute
	}
	return d
}

// News turns on unprompted news pushes for one agent.
//
// There is no hotness threshold here on purpose. The upstream stream is
// already the curated one, and the second cut — is this worth interrupting a
// room over — is the agent's own judgement, which is the whole reason a news
// push goes through a model at all. A number in a config file cannot tell
// "台积电 2nm 提前量产" from the fourth funding round of the week.
type News struct {
	// Feed names the upstream. Empty disables the push however the other
	// fields are set. Today the only value is "aihot".
	Feed string `json:"feed" bson:"feed,omitempty"`
	// Categories keeps only these upstream categories. Empty takes the lot.
	Categories []string `json:"categories" bson:"categories,omitempty"`
	// Batch is how many stories make a push worth sending. Defaults to 3.
	Batch int `json:"batch" bson:"batch,omitempty"`
	// Within is the longest a story waits for company before going out on its
	// own, as a Go duration. Defaults to 30m. This is the other half of the
	// batch rule: without it a slow day never reaches Batch and the room
	// hears nothing at all.
	Within string `json:"within" bson:"within,omitempty"`
	// Every is the poll interval. Defaults to five minutes; anything under a
	// minute is raised to it, because a minute is the upstream cache's own
	// floor and polling faster only burns its rate limit for the same bytes.
	Every string `json:"every" bson:"every,omitempty"`
	// Groups limits which rooms hear the push. Empty means every group the
	// agent belongs to.
	Groups []string `json:"groups" bson:"groups,omitempty"`
}

func (n News) On() bool { return n.Feed != "" }

func (n News) Rooms(joined []string) []string { return rooms(n.Groups, joined) }

// Tuned returns the news config with defaults and floors applied.
func (n News) Tuned() News {
	if n.Batch <= 0 {
		n.Batch = 3
	}
	if d, err := time.ParseDuration(n.Within); err != nil || d <= 0 {
		n.Within = (30 * time.Minute).String()
	}
	d, err := time.ParseDuration(n.Every)
	switch {
	case err != nil || d <= 0:
		d = 5 * time.Minute
	case d < time.Minute:
		d = time.Minute
	}
	n.Every = d.String()
	return n
}

// Interval is how often to poll the feed.
func (n News) Interval() time.Duration {
	d, err := time.ParseDuration(n.Tuned().Every)
	if err != nil {
		return 5 * time.Minute
	}
	return d
}

// Patience is how long a lone story waits for company.
func (n News) Patience() time.Duration {
	d, err := time.ParseDuration(n.Tuned().Within)
	if err != nil {
		return 30 * time.Minute
	}
	return d
}

// Wants reports whether a category passes the filter.
func (n News) Wants(category string) bool {
	if len(n.Categories) == 0 {
		return true
	}
	for _, c := range n.Categories {
		if strings.EqualFold(c, category) {
			return true
		}
	}
	return false
}

// identityColors are the design's agent colours. A client maps them to
// darker equivalents in light mode, so a colour from outside this set would
// not stay readable on white.
var identityColors = []string{
	"#5EE6C0", "#8AB4FF", "#C9A8FF", "#FF9E80", "#E8A33D", "#7FD1FF",
}

// LoadAgents reads the roster file, or builds a single-agent roster from the
// plain environment variables when there is none.
//
// The one-agent form is not a special case to be removed later: a deployment
// with a single assistant should not need a file to say so.
func LoadAgents(path, fallbackUserID, fallbackNickname, fallbackModel string) ([]Agent, error) {
	if path == "" {
		return []Agent{{
			UserID: fallbackUserID, Nickname: fallbackNickname,
			Model: fallbackModel, Color: identityColors[0],
		}}, nil
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("config: agent roster %s: %w", path, err)
	}
	var agents []Agent
	if err := json.Unmarshal(raw, &agents); err != nil {
		return nil, fmt.Errorf("config: agent roster %s: %w", path, err)
	}
	if len(agents) == 0 {
		return nil, fmt.Errorf("config: agent roster %s is empty", path)
	}
	seenID := map[string]bool{}
	seenName := map[string]bool{}
	for i, a := range agents {
		if a.UserID == "" || a.Nickname == "" {
			return nil, fmt.Errorf("config: agent %d needs both user_id and nickname", i)
		}
		if seenID[a.UserID] {
			return nil, fmt.Errorf("config: two agents share user_id %q", a.UserID)
		}
		// Mentions arrive as nicknames as often as ids, so a duplicate name
		// would make it impossible to say which one was called.
		if seenName[a.Nickname] {
			return nil, fmt.Errorf("config: two agents share nickname %q", a.Nickname)
		}
		seenID[a.UserID], seenName[a.Nickname] = true, true
		if agents[i].Model == "" {
			agents[i].Model = fallbackModel
		}
		if agents[i].Color == "" {
			// By position, not by hashing the id: four agents hashed into six
			// colours collide about as often as not, and two agents wearing
			// the same colour in a list of four is the whole problem.
			agents[i].Color = identityColors[i%len(identityColors)]
		}
	}
	return agents, nil
}

// BotEnabled reports whether an agent runtime was configured.
func (c Config) BotEnabled() bool { return c.BotOpencodeURL != "" }

func Load() (Config, error) {
	c := Config{
		Listen:            env("YPTD_LISTEN", "127.0.0.1:7080"),
		MongoURI:          env("YPTD_MONGO_URI", ""),
		MongoDB:           env("YPTD_MONGO_DB", "yptd"),
		OpenIMAPI:         strings.TrimRight(env("YPTD_OPENIM_API", "http://127.0.0.1:10002"), "/"),
		PublicAPI:         strings.TrimRight(env("YPTD_PUBLIC_API", ""), "/"),
		AgentArtDir:       env("YPTD_AGENT_ART_DIR", "/opt/openim/etc/agent-art"),
		OpenIMSecret:      env("YPTD_OPENIM_SECRET", ""),
		OpenIMAdminUserID: env("YPTD_OPENIM_ADMIN", "imAdmin"),
		InviteTTL:         24 * time.Hour,
		PlatformID:        7, // Linux; the TUI overrides per OS at login.

		BotOpencodeURL:      strings.TrimRight(os.Getenv("YPTD_BOT_OPENCODE_URL"), "/"),
		QuoteCLI:            os.Getenv("YPTD_QUOTE_CLI"),
		QuoteHome:           os.Getenv("YPTD_QUOTE_HOME"),
		NewsBase:            os.Getenv("YPTD_NEWS_BASE"),
		BotOpencodeUser:     env("YPTD_BOT_OPENCODE_USER", "yptd"),
		BotOpencodePassword: os.Getenv("YPTD_BOT_OPENCODE_PASSWORD"),
		BotModel:            env("YPTD_BOT_MODEL", "zhipuai/glm-5.3"),
		BotTimeout:          5 * time.Minute,
		BotMaxConcurrent:    2,
	}

	agents, err := LoadAgents(
		os.Getenv("YPTD_BOT_AGENTS"),
		env("YPTD_BOT_USER", "agentbot"),
		env("YPTD_BOT_NICKNAME", "HALX"),
		c.BotModel,
	)
	if err != nil {
		return Config{}, err
	}
	c.Agents = agents

	if raw := os.Getenv("YPTD_BOT_TIMEOUT"); raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("config: YPTD_BOT_TIMEOUT: %w", err)
		}
		c.BotTimeout = d
	}

	if raw := os.Getenv("YPTD_INVITE_TTL"); raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("config: YPTD_INVITE_TTL: %w", err)
		}
		c.InviteTTL = d
	}

	var missing []string
	if c.MongoURI == "" {
		missing = append(missing, "YPTD_MONGO_URI")
	}
	if c.OpenIMSecret == "" {
		missing = append(missing, "YPTD_OPENIM_SECRET")
	}
	if len(missing) > 0 {
		return Config{}, fmt.Errorf("config: missing required environment: %s", strings.Join(missing, ", "))
	}
	return c, nil
}

func env(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
