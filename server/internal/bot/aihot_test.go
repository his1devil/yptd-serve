package bot

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

type hit struct {
	path, cursor, ifNone, fields string
}

// feedServer stands in for aihot.news and records what was asked of it.
func feedServer(t *testing.T, handle func(w http.ResponseWriter, r *http.Request)) (*AIHOT, *[]hit) {
	t.Helper()
	var hits []hit
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits = append(hits, hit{r.URL.Path, r.URL.Query().Get("cursor"), r.Header.Get("If-None-Match"), r.URL.Query().Get("fields")})
		w.Header().Set("Content-Type", "application/json")
		handle(w, r)
	}))
	t.Cleanup(srv.Close)
	return &AIHOT{Base: srv.URL, HTTP: srv.Client()}, &hits
}

func TestFeedFirstCallIsABaselineNotADelivery(t *testing.T) {
	// 第一次只为了拿 cursor。快照是几百条几天前的东西，重启后把它倒进群里
	// 正是这整套设计要防的那件事。
	a, hits := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"items":[{"id":"old","title":"三天前的旧闻"}],"cursor":"c1"}`))
			return
		}
		w.Write([]byte(`{"changes":[{"op":"upsert","item":{"id":"new","title":"新的","links":{"original":"https://x"},"source":{"name":"路透"}}}],"cursor":"c2"}`))
	})
	got, err := a.Fresh(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Fatalf("the baseline must deliver nothing, got %d stories", len(got))
	}
	if (*hits)[0].path != "/api/v1/selected/snapshot" {
		t.Fatalf("the first call should be a snapshot, got %s", (*hits)[0].path)
	}

	got, err = a.Fresh(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Title != "新的" || got[0].Source != "路透" {
		t.Fatalf("the second call should deliver changes: %+v", got)
	}
	if h := (*hits)[1]; h.path != "/api/v1/selected/changes" || h.cursor != "c1" {
		t.Fatalf("changes should carry the snapshot's cursor, got %+v", h)
	}
}

func TestFeedSendsTheETagBackAndHonours304(t *testing.T) {
	calls := 0
	a, hits := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		calls++
		switch {
		case strings.HasSuffix(r.URL.Path, "/snapshot"):
			w.Write([]byte(`{"cursor":"c1"}`))
		case r.Header.Get("If-None-Match") == `"abc"`:
			w.WriteHeader(http.StatusNotModified)
		default:
			// cursor 没动，同一个 URL，下次就能拿 304
			w.Header().Set("ETag", `"abc"`)
			w.Write([]byte(`{"changes":[],"cursor":"c1"}`))
		}
	})
	for i := 0; i < 3; i++ {
		if _, err := a.Fresh(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if last := (*hits)[len(*hits)-1]; last.ifNone != `"abc"` {
		t.Fatalf("a repeat of the same URL must carry If-None-Match, got %q", last.ifNone)
	}
	if calls != 3 {
		t.Fatalf("want 3 requests, got %d", calls)
	}
}

func TestFeedBacksOffOnRateLimit(t *testing.T) {
	calls := 0
	a, _ := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		calls++
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"cursor":"c1"}`))
			return
		}
		w.Header().Set("Retry-After", "120")
		w.WriteHeader(http.StatusTooManyRequests)
	})
	if _, err := a.Fresh(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := a.Fresh(context.Background()); err == nil {
		t.Fatal("a 429 should surface as an error")
	}
	before := calls
	// 被限流之后不许再打网络——这是「有礼貌的客户端」和「没礼貌的」唯一的区别。
	for i := 0; i < 3; i++ {
		if _, err := a.Fresh(context.Background()); err != nil {
			t.Fatalf("a hushed feed should return quietly, got %v", err)
		}
	}
	if calls != before {
		t.Fatalf("no request may go out during the backoff, got %d more", calls-before)
	}
	if d := time.Until(a.quiet); d < 110*time.Second || d > 121*time.Second {
		t.Fatalf("Retry-After: 120 should park polling for about two minutes, got %v", d)
	}
}

func TestFeedStopsWalkingTheBacklog(t *testing.T) {
	// cursor 放了一周的话，hasMore 能一直翻下去；群里也不想一次听一周。
	pages := 0
	a, _ := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"cursor":"c0"}`))
			return
		}
		pages++
		w.Write([]byte(`{"changes":[{"op":"upsert","item":{"id":"i","title":"t"}}],"cursor":"c` + string(rune('0'+pages)) + `","hasMore":true}`))
	})
	if _, err := a.Fresh(context.Background()); err != nil {
		t.Fatal(err)
	}
	got, err := a.Fresh(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if pages > 5 {
		t.Fatalf("draining should be bounded, walked %d pages", pages)
	}
	if len(got) != pages {
		t.Fatalf("every drained page's stories should come back: %d stories from %d pages", len(got), pages)
	}
}

func TestFeedDropsUnusableRows(t *testing.T) {
	a, _ := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"cursor":"c1"}`))
			return
		}
		w.Write([]byte(`{"changes":[
		  {"op":"upsert","item":{"id":"a","title":"  ","summary":"没标题就不是一条新闻"}},
		  {"op":"remove","item":{"id":"gone","title":"撤稿了"}},
		  {"op":"upsert","item":{"id":"b","title":"真的一条","summary":" 摘要 ","category":"ai",
		   "publishedAt":"2026-09-14T07:02:00.000Z","score":63,"selected":true,"originalTitle":null,
		   "links":{"original":"https://r.example/x","aihot":"https://aihot.news/items/b"},
		   "source":{"name":"路透：科技频道"},
		   "attribution":{"name":"AIHOT","url":"https://aihot.news/items/b"}}}
		],"cursor":"c2"}`))
	})
	a.Fresh(context.Background())
	got, err := a.Fresh(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 {
		t.Fatalf("only the usable row should survive: %+v", got)
	}
	it := got[0]
	if it.Title != "真的一条" || it.Source != "路透" || it.URL != "https://r.example/x" ||
		it.Index != "https://aihot.news/items/b" || it.Category != "ai" || it.Summary != "摘要" {
		t.Fatalf("fields should map straight across: %+v", it)
	}
	if it.Published.IsZero() {
		t.Fatal("publishedAt should parse")
	}
}

func TestFeedSurvivesAnUnparseablePublishTime(t *testing.T) {
	a, _ := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"cursor":"c1"}`))
			return
		}
		w.Write([]byte(`{"changes":[{"op":"upsert","item":{"id":"a","title":"t","publishedAt":"昨天"}}],"cursor":"c2"}`))
	})
	a.Fresh(context.Background())
	got, _ := a.Fresh(context.Background())
	if len(got) != 1 || !got[0].Published.IsZero() {
		t.Fatalf("a time we cannot read should be zero, not a guess: %+v", got)
	}
	// zero 时间在 Digest 里就是不写时间，而不是写「56 年前」
	if strings.Contains(Digest(got, time.Now()), "年前") {
		t.Fatal("an unknown time must not be rendered")
	}
}

func TestFeedBaselineMustNotPinMinimalFields(t *testing.T) {
	// cursor 把字段集编在里面（"f":"minimal"）。快照图省事要了 minimal，
	// 之后每一次 changes 都跟着缩水——而且看起来一切正常，只是全是空的。
	a, hits := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`{"cursor":"c1"}`))
	})
	if _, err := a.Fresh(context.Background()); err != nil {
		t.Fatal(err)
	}
	if got := (*hits)[0].fields; got != "" {
		t.Fatalf("the baseline must not pin a field set, asked for %q", got)
	}
}

// 线上真实的撤稿是 op:"remove"（不是 delete）。代码本来就是「不是 upsert 一律跳过」，
// 但测试如果断言一个现实里不存在的值，过了也说明不了什么。
func TestFeedSkipsRetractions(t *testing.T) {
	a, _ := feedServer(t, func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/snapshot") {
			w.Write([]byte(`{"cursor":"c1"}`))
			return
		}
		w.Write([]byte(`{"changes":[{"op":"remove","item":{"id":"x","title":"撤稿了"}}],"cursor":"c2"}`))
	})
	a.Fresh(context.Background())
	got, err := a.Fresh(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Fatalf("a retraction is not news: %+v", got)
	}
}

func TestPublisherDropsTheFeedsQualifier(t *testing.T) {
	for in, want := range map[string]string{
		"Sam Altman：Blog（RSS）":           "Sam Altman",
		"蚂蚁 inclusionAI：HuggingFace 新模型": "蚂蚁 inclusionAI",
		"路透":           "路透",
		"Ars Technica": "Ars Technica",
		"：只有限定词":       "：只有限定词",
	} {
		if got := publisher(in); got != want {
			t.Fatalf("%q should cite as %q, got %q", in, want, got)
		}
	}
}
