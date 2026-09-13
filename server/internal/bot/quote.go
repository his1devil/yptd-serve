package bot

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// Quote is one symbol's state right now, reduced to what a watch decides on.
type Quote struct {
	Symbol string
	Last   float64
	// ChangePct is the move away from the previous close, in percent.
	ChangePct float64
	// Status is Longbridge's trading status, e.g. Normal or Suspend. A
	// halted symbol keeps reporting its last price, so the watch skips it
	// rather than alerting on a number that cannot move.
	Status string
}

// Halted reports whether this symbol is not trading normally.
func (q Quote) Halted() bool { return q.Status != "" && !strings.EqualFold(q.Status, "Normal") }

// Quotes is where a watch gets its numbers. An interface so the decision
// logic can be tested without spawning anything.
type Quotes interface {
	Quotes(ctx context.Context, symbols []string) ([]Quote, error)
}

// Longbridge reads quotes through the `longbridge` CLI, the same binary the
// agents' skills use. It is a process spawn per poll, which is why the watch
// interval has a floor.
//
// The CLI carries its own credentials in the running user's home; this
// service never sees them. An expired login therefore surfaces here as an
// error on every poll, not as silently stale numbers.
type Longbridge struct {
	// Bin is the executable; empty means "longbridge" on PATH.
	Bin string
	// Home is the HOME the CLI reads its login from (it keeps credentials in
	// $HOME/.longbridge). Set it when this service runs as a different user
	// than the one that ran `longbridge auth login` — otherwise every quote
	// comes back "Not authenticated" and the watch stays silent forever.
	// Empty inherits the service's own HOME.
	Home string
	// Timeout bounds one invocation.
	Timeout time.Duration
}

func (l Longbridge) Quotes(ctx context.Context, symbols []string) ([]Quote, error) {
	if len(symbols) == 0 {
		return nil, nil
	}
	bin := l.Bin
	if bin == "" {
		bin = "longbridge"
	}
	timeout := l.Timeout
	if timeout <= 0 {
		timeout = 30 * time.Second
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	args := append([]string{"quote"}, symbols...)
	args = append(args, "--format", "json")
	cmd := exec.CommandContext(ctx, bin, args...)
	if l.Home != "" {
		cmd.Env = append(os.Environ(), "HOME="+l.Home)
	}
	out, err := cmd.Output()
	if err != nil {
		// The CLI prints its reason on stderr; carry it, because "not
		// authenticated" and "no such symbol" need very different fixes.
		var ee *exec.ExitError
		if errors.As(err, &ee) && len(ee.Stderr) > 0 {
			return nil, fmt.Errorf("longbridge quote: %w: %s", err, oneLine(string(ee.Stderr)))
		}
		return nil, fmt.Errorf("longbridge quote: %w", err)
	}
	return parseQuotes(out)
}

// parseQuotes reads the CLI's `--format json` array. Numbers come back as
// strings, and a field the CLI could not fill comes back empty rather than
// absent, so every one is parsed leniently and a symbol missing a usable
// price is dropped instead of alerting on a zero.
func parseQuotes(raw []byte) ([]Quote, error) {
	var rows []struct {
		Symbol    string `json:"symbol"`
		Last      string `json:"last"`
		ChangePct string `json:"change_percentage"`
		Status    string `json:"status"`
	}
	if err := json.Unmarshal(raw, &rows); err != nil {
		return nil, fmt.Errorf("longbridge quote: decode: %w", err)
	}
	out := make([]Quote, 0, len(rows))
	for _, r := range rows {
		if r.Symbol == "" {
			continue
		}
		last, errL := strconv.ParseFloat(strings.TrimSpace(r.Last), 64)
		pct, errP := strconv.ParseFloat(strings.TrimSpace(r.ChangePct), 64)
		if errL != nil || errP != nil {
			continue
		}
		out = append(out, Quote{Symbol: r.Symbol, Last: last, ChangePct: pct, Status: r.Status})
	}
	return out, nil
}
