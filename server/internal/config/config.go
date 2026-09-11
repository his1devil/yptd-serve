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
		OpenIMSecret:      env("YPTD_OPENIM_SECRET", ""),
		OpenIMAdminUserID: env("YPTD_OPENIM_ADMIN", "imAdmin"),
		InviteTTL:         24 * time.Hour,
		PlatformID:        7, // Linux; the TUI overrides per OS at login.

		BotOpencodeURL:      strings.TrimRight(os.Getenv("YPTD_BOT_OPENCODE_URL"), "/"),
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
