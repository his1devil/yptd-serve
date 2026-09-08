# yptd-tui

对接 OpenIM 的终端 IM 客户端，Rust + [ratatui](https://ratatui.rs)。

交互与视觉设计参考 [concord](https://github.com/chojs23/concord)（Discord 的 TUI 客户端），
但代码从零编写，不含其 GPL 代码。

> **当前状态：已接真实后端。** `yptd login` 用邀请码登录一次，之后 `yptd`
> 直接进入：会话列表、历史、群成员、发消息、引用回复、@ 提及、建群拉人、私聊、
> 实时推送都走 OpenIM。
> 没账号也能跑 `yptd --mock` 看渲染。

---

## 组成

```
crates/yptd-tui   Rust 终端客户端（本仓库主体）
sidecar/          Go 边车：把 openim-sdk-core 包成本地 Unix socket 上的 NDJSON 服务
server/           Go 服务端 yptd-server：邀请码、设备凭据、换 OpenIM token；自带管理 CLI
```

OpenIM 的 WebSocket 帧用 Go 的 `encoding/gob` 编码，Rust 没有实现，
所以 SDK 逻辑留在 Go 里，TUI 通过边车拿数据。边车与 `yptd` 是两个二进制，
`yptd` 启动时自动拉起并在退出时收掉，用户不需要知道它存在。

---

## 安装与登录

需要 Rust 1.90+、Go 1.24+ 和 C 编译器（边车里的 sqlite 走 CGO）。

```sh
git clone https://github.com/his1devil/yptd-serve.git yptd-tui
cd yptd-tui
make install            # 构建 release 并装到 ~/.cargo/bin：yptd + yptd-sidecar
```

找管理员要一个邀请码（形如 `YPTD-XXXX-XXXX`，24 小时内有效，只能用一次）：

```sh
yptd login              # 邀请码 → 昵称 → 用户名（可留空）
yptd                    # 以后每次就这一条
```

登录时服务端返回一个**设备凭据**，存在 `~/.yptd/credentials.toml`（0600）。
之后每次启动用它换一个新的 OpenIM token，token 本身不落盘。
`yptd logout` 删掉本机凭据；管理员 `yptd-server user revoke <id>` 可远程吊销。

不想装到 PATH 的话，`make` 之后 `./target/debug/yptd` 也行——边车放在同目录即可，
也可用 `$YPTD_SIDECAR` 指定路径。所有文件都在 `~/.yptd/`（或 `$YPTD_HOME`）。

### 不进界面的自检

```sh
yptd doctor                       # 登录、起边车、拉会话与历史，监听 8 秒推送
yptd doctor "hello"               # 同上，并向最近的会话发一条
yptd doctor --frame 120x32        # 把真实数据渲染成一帧文本，ssh 里也能看
yptd doctor --new 群名 lina wuhao   # 建群并拉人，读回成员
yptd doctor --reply lina 文本       # 引用最新一条并 @ 这个人，打印回显
```

---

## 只看渲染

```sh
cargo run -- --mock               # 内置示例数据，不连服务器
cargo run -- --snapshot 112x28 top       # 纯文本
cargo run -- --ansi 112x28 top           # 真 ANSI，用你自己终端的配色
cargo run -- --html 112x28 top dark > frame.html
```

参数：`--<模式> <宽>x<高> [场景] [配色]`

| 场景 | 看什么 |
| --- | --- |
| `live` | 默认。跟随最新消息，含附件、发送失败、ghost 回复 |
| `top` | 会话顶部。日期分隔线、系统通知、提及高亮、reaction、已读回执 |
| `code` | 围栏代码块与 syntect 语法高亮 |
| `nav` | 焦点在会话栏，一个分类被折叠 |
| `insert` | 输入模式，composer 激活 |
| `picker` | 邀请弹层盖在会话上，勾了两个人 |
| `reply` | 正在写一条带引用和 @ 的回复 |

---

## 键位

| 键 | 作用 |
| --- | --- |
| `1` `2` `3` | 聚焦 会话 / 消息 / 成员 |
| `Tab` `S-Tab`、`h` `l` | 切换焦点 |
| `j` `k`、`C-n` `C-p` | 移动**选择光标**，视口跟随 |
| `J` `K` | 移动**视口**，光标不动 |
| `C-d` `C-u` | 半页滚动 |
| `g` `G` | 跳到顶部 / 最新（`G` 重新开启自动跟随） |
| `Enter` | 打开会话；在分类上则折叠 |
| `z` | 折叠 / 展开分类 |
| `r` | 引用选中的消息并进入 INSERT |
| `i` | 进入 INSERT（`Esc` 退出） |
| `@` | INSERT 里：弹出本会话成员名单，选中即插入 |
| `Enter` / `S-Enter` | INSERT 里：发送 / 换行 |
| `C-a` `C-e` `C-u` `C-k` `C-w`、`M-←` `M-→` | INSERT 里的行编辑 |
| `:` | 进入 COMMAND（`Esc` 退出） |
| `q` | 退出 |

鼠标：点击切换焦点与选中，双击会话打开，滚轮滚动各栏。

### 命令（`:` 进入，`Enter` 执行）

| 命令 | 作用 |
| --- | --- |
| `:new 群名` | 建群（自己是群主），打开后立刻弹出邀请名单 |
| `:invite` | 在当前群弹出邀请名单；`:invite lina wuhao` 直接拉这几个人 |
| `:dm` | 弹出名单选一个人私聊；`:dm lina` 直接开 |
| `:help` | 命令一览 |
| `:q` | 退出 |

`Esc` 一次剥一层：先取消引用，再退出 INSERT。切换会话会丢掉引用和已选的提及——
它们属于刚才那个会话，带过去就会引用别人看不见的消息。

弹出的名单：直接打字过滤（昵称或用户名，不分大小写），`↑` `↓` / `C-n` `C-p` 移动，
`Space` / `Tab` 勾选，`Enter` 确定（没勾任何人时取光标所在的那个），`Esc` 取消。
名单来自服务端花名册（`GET /v1/users`，凭设备凭据），已在群里的人和自己不会出现。

`/` 留给 composer 里的 agent 命令，`:` 留给客户端命令——两者语义不同，刻意分开。

---

## 响应式

横向空间不够时按优先级收起，无需手动调：

| 终端宽度 | 布局 |
| --- | --- |
| ≥ 100 | 会话 + 消息 + 成员 |
| 72–99 | 会话 + 消息 |
| < 72 | 仅消息 |

消息栏永远保留，且不低于 40 列——低于这个宽度代码块就没法读了。

---

## 运行时的几条约定

- **一个通道，两个来源。** 终端输入和边车事件各占一个线程，都推进同一个 `mpsc`，
  主循环只做 `recv()`，不轮询。键盘和鼠标立刻重绘；后台事件先合并 40 ms 再画一帧，
  且只有当变化落在屏幕上（当前会话的消息、侧栏、连接状态）才画。
- **登录后等同步。** SDK 的 `Login` 在 socket 建好时就返回，会话与群是之后异步拉进本地库的；
  客户端等到 `OnSyncServerFinish`（最多 20 秒，连接后静默 1.5 秒也算）再读列表，
  否则第一帧是空的。
- **发出去的消息以 SDK 回显为准。** 不在本地猜 ID；`send` 返回的完整消息进快照。
- **历史按会话懒拉一次**（最新 60 条）并标记已读；同步完成后清掉缓存重新拉。
- **弹层独占键盘。** 名单打开时所有按键归它，`q` 是过滤字符而不是退出；鼠标点在下面的栏上不生效。
- **私聊会话本地先建行。** `:dm` 用 SDK 的规则算出 `si_<a>_<b>`（按字典序排的两个 id），
  服务端在第一条消息发出后才有这条会话，之后推来的行会落在同一个 id 上。
- **引用和 @ 是同一条消息。** SDK 把被引用的原文塞在 at-text 元素里，
  所以「回复并 @」发出去是一条 contentType 106，而不是两条；读的时候两个位置都要认。
- **发送前重新核对提及。** 名单里选过的人，如果名字又被删掉了，就不该收到通知；
  发送时按最终文本里还在不在 `@昵称` 过一遍，并按 id 去重。

---

## 测试

```sh
make test           # cargo test + go test
```

值得一看的几条，它们守着容易写错的地方：

- `selecting_a_message_does_not_change_how_many_lines_it_occupies`
  —— 选中态只加左侧竖条、不改内容宽度。否则每次移动光标都要重排换行，视口就守不住了。
- `wrapping_never_loses_or_duplicates_text` —— 4..40 列全宽度往返一致。
- `cjk_is_measured_two_cells_wide` —— 中文按显示宽度而非字符数换行。
- `spans_survive_a_rewrite_that_lengthens_the_text` —— 文本改写后标注跟着平移，不重新解析。
- `scrolling_to_a_message_keeps_its_own_divider_in_view` —— 分隔线属于它引出的那条消息。
- `a_garbage_send_time_still_yields_distinct_ids` —— `sendTime` 缺失也不会让两条消息撞 ID。
- `sending_in_mock_mode_keeps_the_draft_and_explains` —— 发送失败不能吞掉草稿。
- `keys_go_to_the_picker_while_it_is_open` —— 弹层开着时 `q` 是过滤，不是退出。
- `dm_opens_a_direct_conversation_on_the_sdk_derived_id` —— 本地建的私聊行和服务端推的落在同一个 id。
- `a_mention_deleted_from_the_draft_does_not_notify` —— 删掉名字就不再通知那个人。
- `a_reply_that_also_mentions_someone_carries_both` —— 引用藏在 at-text 里也要读出来。
- `r_starts_a_reply_and_escape_drops_it_before_leaving_insert` —— Esc 一次只剥一层。

---

## 结构

```
crates/
  tui-theme/     具名高亮组 + link 继承；颜色与边框几何分离
  tui-richtext/  纯文本 + 字节区间标注；改写重映射、宽度感知换行、Markdown 子集
  tui-textedit/  composer 的文本缓冲：字素边界、词跳、纵向移动
  im-model/      OpenIM 语义的领域模型 + 合成雪花 ID + mock 快照 + SDK JSON 翻译
  im-sidecar/    边车进程管理与 NDJSON 请求/事件通道
  yptd-tui/      三栏骨架、消息行渲染、双光标、图片、离屏截帧、登录与会话、命令与弹层
```

依赖方向单向：`yptd-tui` → `im-sidecar` / `im-model` / `tui-*`，反向不可见。
`im-model::translate` 是 SDK JSON 进入领域模型的唯一入口；`App` 从不看到 SDK 字段名。

### 两个设计要点

**主题只认名字，不认颜色。** 渲染代码写 `theme.style(HG::MessageAuthor)`，
从不写 `Color::Cyan`。90 个高亮组之间可 `link` 继承——改一个 `Muted`，
所有次要文本一起变。默认配色全部用 16 个 ANSI 名，所以客户端跟随你终端已有的配色方案，
而不是自带一套跟你抢。

**消息 ID 自带时间。** OpenIM 用 `clientMsgID`(UUID) + 每会话 `seq` 标识消息，
两者都不能跨会话排序。把发送时间打进高 42 位，一次拿到三样东西：
与真实时间一致的全序、不必单独存储的时间戳、以及便宜到能当 `BTreeMap` 键的 `Copy` 类型。
低 22 位是毫秒内计数器；`clientMsgID` ↔ ID 的映射在 `Interner` 里。

---

## 服务端

部署与运维见 `server/`。管理员常用：

```sh
yptd-server invite new "给谁的"      # 生成邀请码
yptd-server invite list -a
yptd-server user list
yptd-server user disable <userID>   # 停用（同时吊销设备凭据）
yptd-server user revoke <userID>    # 只吊销凭据，账号保留
yptd-server check                   # Mongo 与 OpenIM 连通性
```

---

## 尚未实现

发图片、reaction、撤回、重发失败的消息、退群 / 踢人、模糊会话切换器、
leader 键提示窗、可配置键位。凭据先放 0600 文件，钥匙串以后再说。

## 许可

MIT OR Apache-2.0
