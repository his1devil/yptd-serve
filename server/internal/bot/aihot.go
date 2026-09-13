package bot

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Feed is where a news push gets its stories. An interface so the batching
// and the wording can be tested without touching the network.
type Feed interface {
	// Fresh returns what has appeared since the last call. The first call
	// returns nothing: see AIHOT.Fresh.
	Fresh(ctx context.Context) ([]NewsItem, error)
}

// AIHOT reads the curated stream at aihot.news.
//
// The protocol is snapshot-then-changes: one snapshot hands back a cursor,
// and every later call passes that cursor to get only what is new. There is
// no webhook and no stream, so this polls — politely, because the upstream
// asks for exactly that: honour the cache with If-None-Match, do not poll
// faster than its s-maxage, and back off when it says 429.
type AIHOT struct {
	// Base is the origin, default https://aihot.news.
	Base string
	// HTTP is the client, default one with a sane timeout.
	HTTP *http.Client

	mu     sync.Mutex
	cursor string
	// etag belongs to the last changes URL. It is dropped whenever the cursor
	// moves, because the ETag describes one URL and the cursor is in it.
	etag string
	// quiet is when a 429's Retry-After expires. Until then Fresh returns
	// without touching the network: retrying through a rate limit is how a
	// polite client becomes an impolite one.
	quiet time.Time
}

// Fresh returns the stories that appeared since the previous call.
//
// The very first call is a baseline, not a delivery: it takes a snapshot to
// learn the cursor and returns nothing. A snapshot is hundreds of stories
// going back days, and dumping that into a chat room on the first poll after
// a restart is the one failure this whole design exists to avoid.
func (a *AIHOT) Fresh(ctx context.Context) ([]NewsItem, error) {
	a.mu.Lock()
	cursor, etag, quiet := a.cursor, a.etag, a.quiet
	a.mu.Unlock()

	if !quiet.IsZero() && time.Now().Before(quiet) {
		return nil, nil
	}
	if cursor == "" {
		c, err := a.baseline(ctx)
		if err != nil {
			return nil, err
		}
		a.mu.Lock()
		a.cursor, a.etag = c, ""
		a.mu.Unlock()
		return nil, nil
	}
	return a.changes(ctx, cursor, etag)
}

// baseline takes a snapshot only to learn the cursor, so it asks for the
// smallest page there is.
//
// It must NOT ask for fields=minimal, tempting as that is for a page we throw
// away: the cursor encodes the field set ("f":"minimal") and every later
// changes call inherits it, so the stories would arrive stripped of summary,
// links and source — and would arrive looking fine, just empty.
func (a *AIHOT) baseline(ctx context.Context) (string, error) {
	var out struct {
		Cursor string `json:"cursor"`
	}
	q := url.Values{"limit": {"1"}}
	if _, err := a.get(ctx, "/api/v1/selected/snapshot", q, "", &out); err != nil {
		return "", err
	}
	if out.Cursor == "" {
		return "", fmt.Errorf("aihot snapshot: no cursor")
	}
	return out.Cursor, nil
}

// changes drains what is new, a page at a time. Bounded: a cursor that has
// been stale for a week could otherwise walk the whole backlog in one poll,
// and a room does not want to hear a week at once either.
func (a *AIHOT) changes(ctx context.Context, cursor, etag string) ([]NewsItem, error) {
	const maxPages = 5
	var all []NewsItem
	for page := 0; page < maxPages; page++ {
		var out struct {
			// 顶层键是 changes，不是 items——items 是 snapshot 的。
			Changes []rawChange `json:"changes"`
			Cursor  string      `json:"cursor"`
			HasMore bool        `json:"hasMore"`
		}
		q := url.Values{"cursor": {cursor}, "limit": {"50"}}
		res, err := a.get(ctx, "/api/v1/selected/changes", q, etag, &out)
		if err != nil {
			return all, err
		}
		if res.notModified {
			return all, nil
		}
		for _, c := range out.Changes {
			// 只推新增和更新。撤稿是撤稿，不是新闻。
			if c.Op != "" && !strings.EqualFold(c.Op, "upsert") {
				continue
			}
			if it, ok := c.Item.item(); ok {
				all = append(all, it)
			}
		}
		// A response without a cursor cannot be paged from; keep the one we
		// have so the next poll retries rather than falling back to a
		// snapshot and losing everything in between.
		if out.Cursor == "" {
			break
		}
		moved := out.Cursor != cursor
		cursor = out.Cursor
		a.mu.Lock()
		a.cursor = cursor
		// Only an unmoved cursor keeps its ETag: it is the same URL.
		if moved {
			a.etag = ""
		} else {
			a.etag = res.etag
		}
		a.mu.Unlock()
		if !out.HasMore || !moved {
			break
		}
		etag = ""
	}
	return all, nil
}

type result struct {
	etag        string
	notModified bool
}

func (a *AIHOT) get(ctx context.Context, path string, q url.Values, etag string, out any) (result, error) {
	base := a.Base
	if base == "" {
		base = "https://aihot.news"
	}
	client := a.HTTP
	if client == nil {
		client = &http.Client{Timeout: 20 * time.Second}
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(base, "/")+path+"?"+q.Encode(), nil)
	if err != nil {
		return result{}, err
	}
	req.Header.Set("Accept", "application/json")
	if etag != "" {
		req.Header.Set("If-None-Match", etag)
	}
	res, err := client.Do(req)
	if err != nil {
		return result{}, fmt.Errorf("aihot %s: %w", path, err)
	}
	defer res.Body.Close()

	switch {
	case res.StatusCode == http.StatusNotModified:
		return result{etag: etag, notModified: true}, nil
	case res.StatusCode == http.StatusTooManyRequests:
		a.hush(res.Header.Get("Retry-After"))
		return result{}, fmt.Errorf("aihot %s: rate limited", path)
	case res.StatusCode == http.StatusConflict, res.StatusCode == http.StatusGone,
		res.StatusCode == http.StatusBadRequest:
		// The cursor was rejected — too old after a long outage, or from a
		// schema that no longer exists. Drop it so the next poll takes a
		// fresh baseline. Without this the watcher errors on every poll
		// forever, since nothing else ever clears the cursor. Whatever
		// happened during the outage is skipped, which is the right trade: a
		// room does not want a week of backlog at once either.
		a.mu.Lock()
		a.cursor, a.etag = "", ""
		a.mu.Unlock()
		return result{}, fmt.Errorf("aihot %s: cursor rejected (%s), re-syncing", path, res.Status)
	case res.StatusCode != http.StatusOK:
		return result{}, fmt.Errorf("aihot %s: %s", path, res.Status)
	}
	if err := json.NewDecoder(res.Body).Decode(out); err != nil {
		return result{}, fmt.Errorf("aihot %s: decode: %w", path, err)
	}
	return result{etag: res.Header.Get("ETag")}, nil
}

// hush parks polling for as long as the server asked. Retry-After is either
// seconds or an HTTP date; an unparseable one still gets a minute, because a
// 429 we cannot read is still a 429.
func (a *AIHOT) hush(retryAfter string) {
	wait := time.Minute
	if s := strings.TrimSpace(retryAfter); s != "" {
		if n, err := strconv.Atoi(s); err == nil && n > 0 {
			wait = time.Duration(n) * time.Second
		} else if t, err := http.ParseTime(s); err == nil {
			if d := time.Until(t); d > 0 {
				wait = d
			}
		}
	}
	if wait > time.Hour {
		wait = time.Hour
	}
	a.mu.Lock()
	a.quiet = time.Now().Add(wait)
	a.mu.Unlock()
}

// rawChange is one entry of the changes feed: an operation and the story it
// happened to.
type rawChange struct {
	Op   string  `json:"op"`
	Item rawItem `json:"item"`
}

// rawItem is the upstream shape. Everything in it is third-party text: it is
// data to be rendered and shown, never instructions to follow.
type rawItem struct {
	ID       string `json:"id"`
	Title    string `json:"title"`
	Summary  string `json:"summary"`
	Category string `json:"category"`
	Links    struct {
		AIHOT    string `json:"aihot"`
		Original string `json:"original"`
	} `json:"links"`
	Source struct {
		Name string `json:"name"`
		URL  string `json:"url"`
	} `json:"source"`
	PublishedAt string `json:"publishedAt"`
}

func (r rawItem) item() (NewsItem, bool) {
	title := strings.TrimSpace(r.Title)
	if title == "" {
		return NewsItem{}, false
	}
	var at time.Time
	for _, layout := range []string{time.RFC3339, time.RFC3339Nano, "2006-01-02T15:04:05Z0700", time.RFC1123Z} {
		if t, err := time.Parse(layout, r.PublishedAt); err == nil {
			at = t
			break
		}
	}
	return NewsItem{
		ID:        r.ID,
		Title:     title,
		Source:    publisher(r.Source.Name),
		URL:       strings.TrimSpace(r.Links.Original),
		Index:     strings.TrimSpace(r.Links.AIHOT),
		Category:  strings.TrimSpace(r.Category),
		Published: at,
		Summary:   strings.TrimSpace(r.Summary),
	}, true
}

// publisher trims the feed's own qualifier off a source label. Upstream names
// a source by what it is as much as who it is — "Sam Altman：Blog（RSS）",
// "蚂蚁 inclusionAI：HuggingFace 新模型" — and a citation wants the who. The
// full-width colon is the feed's separator; a half-width one is left alone,
// since that is what appears inside real names and URLs.
func publisher(name string) string {
	name = strings.TrimSpace(name)
	if i := strings.Index(name, "："); i > 0 {
		return strings.TrimSpace(name[:i])
	}
	return name
}
