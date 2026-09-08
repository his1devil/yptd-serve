// Package config loads yptd-server settings from the environment.
//
// Environment rather than a config file, because every value here is either a
// secret or a deployment address: both belong in the unit file or the shell
// that starts the process, not in a file that is easy to commit by accident.
package config

import (
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
}

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
