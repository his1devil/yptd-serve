# opencode 侧的文件

这里放的是部署到服务器 `/opt/yptd-bot/.config/opencode/` 的 agent 人设和 skill，
只有和 yptd-serve 代码有耦合的那几个（skill 调的命令就在本仓库 `cmd/` 里）。
HALX、Charlie 等纯人设文件只在服务器上。

改完要 `systemctl restart yptd-opencode`。

- `agent/dummy.md` — Dummy：记 bug 和需求
- `skills/yptd-notes/SKILL.md` — `yptd-note` 命令怎么用；所有 agent 都能加载
