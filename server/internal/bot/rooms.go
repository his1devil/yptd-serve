package bot

import (
	"context"

	"github.com/his1devil/yptd/server/internal/config"
)

// ConfigStore is where per-group agent settings live.
type ConfigStore interface {
	AgentWatch(ctx context.Context, agentID string) (map[string]config.Watch, error)
	AgentNews(ctx context.Context, agentID string) (map[string]config.News, error)
}

// StoredRooms answers "what should this agent watch, per group" by looking at
// what each group configured, falling back to the roster file.
//
// agents.json 里那份降级成默认值：群里没配过就用它。这样现有部署一行迁移都不用写，
// 新拉 agent 进去的群也有个合理的起点，而不是一片空白等人来填。
type StoredRooms struct {
	Store ConfigStore
}

func (r StoredRooms) WatchFor(ctx context.Context, agentID string) (map[string]config.Watch, error) {
	return r.Store.AgentWatch(ctx, agentID)
}

// NewsRooms is the same idea for the news push.
func (r StoredRooms) NewsRooms(ctx context.Context, agent config.Agent, joined []string) (map[string]config.News, error) {
	byGroup, err := r.Store.AgentNews(ctx, agent.UserID)
	if err != nil {
		return nil, err
	}
	out := make(map[string]config.News, len(joined))
	for _, g := range joined {
		n, ok := byGroup[g]
		if !ok {
			if len(agent.News.Rooms([]string{g})) == 0 {
				continue
			}
			n = agent.News
		}
		if !n.On() {
			continue
		}
		out[g] = n
	}
	return out, nil
}
