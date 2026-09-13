package bot

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"testing"
	"time"
)

// 打真源站，确认结构体标签和真实载荷对得上——字段名对不上时接口照样 200，
// 只是解出来全是空的，这类 bug 不会自己喊疼。LIVE=1 才跑。
func TestLiveAIHOT(t *testing.T) {
	if os.Getenv("LIVE") == "" {
		t.Skip("set LIVE=1")
	}
	a := &AIHOT{}
	if _, err := a.Fresh(context.Background()); err != nil {
		t.Fatal("baseline:", err)
	}
	if a.cursor == "" {
		t.Fatal("baseline produced no cursor")
	}

	res, err := http.Get("https://aihot.news/api/v1/selected/snapshot?limit=5")
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	var out struct {
		Items []rawItem `json:"items"`
	}
	if err := json.NewDecoder(res.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	if len(out.Items) == 0 {
		t.Fatal("no items came back")
	}
	var got []NewsItem
	for _, r := range out.Items {
		if it, ok := r.item(); ok {
			got = append(got, it)
		}
	}
	if len(got) == 0 {
		t.Fatal("every live row was dropped — the struct tags do not match the payload")
	}
	for _, it := range got {
		if it.Title == "" || it.Source == "" || it.URL == "" || it.Published.IsZero() {
			t.Errorf("a live story came back half-decoded: %+v", it)
		}
	}
	for i := range got {
		got[i].Take = "（这里是 JOMO 写的一句判断。）"
	}
	n := len(got)
	if n > 2 {
		n = 2
	}
	t.Logf("解出 %d 条，渲染出来是：\n%s", len(got), Digest(got[:n], time.Now()))
}
