package bot

import (
	"fmt"
	"strings"
	"time"
)

// NewsItem is one story, flattened from whatever the feed hands back. The
// field names are ours, not the upstream JSON's — the fetcher maps into this
// so the message format does not move every time a feed renames something.
type NewsItem struct {
	ID string
	// Title is the headline as it should read in the room.
	Title string
	// Source is the publisher — 路透、OpenAI 博客. This is the citation: it is
	// what the room is being told the story comes from, so it is always the
	// original publisher and never the aggregator that surfaced it.
	Source string
	// URL is the publisher's own page. The citation links here.
	URL string
	// Index is the aggregator's own entry for this story. Carried because the
	// feed hands it over and a "打开收录页" action may want it one day — it is
	// deliberately NOT in the message: the citation is the publisher, and a
	// second link to whoever happened to surface the story is noise in a room.
	// Internal use is free under the feed's terms; an outward-facing product
	// built on it needs written permission, and that is where crediting the
	// aggregator would become a question again.
	Index string
	// Category is the upstream's own bucket, used to filter before a batch
	// ever reaches the model. Not shown: the headline already says what it is.
	Category string
	// Published is when the publisher put it out.
	Published time.Time
	// Summary is the aggregator's abstract, used only when Take is empty.
	Summary string
	// Take is the agent's own one-line read: why this matters, what changes.
	// It is the reason the push exists — relaying headlines needs no agent.
	Take string
}

// Digest writes the message a news push posts.
//
// Three lines per story, in the order you would want to read them: what
// happened, what it means, who published it. One story gets no heading —
// same rule as a quote alert, a heading over a single entry is ceremony.
//
// The markup is only what the client's Rich renderer parses: **bold** and
// [text](url). Lists and headings are not parsed there, so the heading is a
// bold line and the blank line between stories is a real newline. Bold must
// not wrap a link — Rich's bold token swallows it and the link stops being a
// link — which is why the headline is bold and the citation is the link.
//
// When Take is empty (the model call failed, or none was asked for) the
// aggregator's own summary stands in, so a push is never a bare list of URLs.
// Both are descriptive and both are covered by the citation below them.
func Digest(items []NewsItem, now time.Time) string {
	var out []string
	for _, it := range items {
		if it.Title == "" {
			continue
		}
		out = append(out, story(it, now))
	}
	if len(out) == 0 {
		return ""
	}
	if len(out) == 1 {
		return out[0]
	}
	return fmt.Sprintf("**科技热点** · %d 条\n\n%s", len(out), strings.Join(out, "\n\n"))
}

func story(it NewsItem, now time.Time) string {
	var b strings.Builder
	fmt.Fprintf(&b, "**%s**", it.Title)

	if line := firstLine(it.Take, it.Summary); line != "" {
		b.WriteString("\n" + line)
	}

	// 出处这行就是引用，挂的是发布方自己的名字和它自己的链接。中间那个把稿子
	// 捞出来的聚合站不出现在这里——读者要判断的是「路透说的」还是「某博客说的」。
	var cite []string
	if it.Source != "" {
		if it.URL != "" {
			cite = append(cite, fmt.Sprintf("[%s](%s)", it.Source, it.URL))
		} else {
			cite = append(cite, it.Source)
		}
	} else if it.URL != "" {
		cite = append(cite, fmt.Sprintf("[原文](%s)", it.URL))
	}
	if when := since(it.Published, now); when != "" {
		cite = append(cite, when)
	}
	if len(cite) > 0 {
		b.WriteString("\n" + strings.Join(cite, " · "))
	}
	return b.String()
}

// firstLine takes the first non-empty candidate and flattens it to one line.
// A take that arrives as a paragraph would otherwise break the three-line
// shape the whole format rests on.
func firstLine(candidates ...string) string {
	for _, c := range candidates {
		c = strings.TrimSpace(c)
		if c == "" {
			continue
		}
		if i := strings.IndexAny(c, "\n\r"); i >= 0 {
			c = strings.TrimSpace(c[:i])
		}
		if c != "" {
			return c
		}
	}
	return ""
}

// since writes how long ago something was published, coarsely. Coarse on
// purpose: "2 小时前" is what a reader needs to judge staleness, and a precise
// timestamp would just be a second clock next to the message's own.
func since(t, now time.Time) string {
	if t.IsZero() {
		return ""
	}
	d := now.Sub(t)
	switch {
	case d < 0:
		// 源站时钟比我们快一点，别写成「-1 分钟前」。
		return "刚刚"
	case d < time.Minute:
		return "刚刚"
	case d < time.Hour:
		return fmt.Sprintf("%d 分钟前", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%d 小时前", int(d.Hours()))
	default:
		return t.Local().Format("1月2日")
	}
}
