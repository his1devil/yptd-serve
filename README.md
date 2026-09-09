# yptd-tui

对接 OpenIM 的终端 IM 客户端，Rust + [ratatui](https://ratatui.rs)。

交互与视觉设计参考 [concord](https://github.com/chojs23/concord)（Discord 的 TUI 客户端），
但代码从零编写，不含其 GPL 代码。

> **当前状态：已接真实后端。** `yptd login` 用邀请码登录一次，之后 `yptd`
> 直接进入：会话列表、历史、群成员、发消息、引用回复、@ 提及、发图片、建群拉人、
> 私聊、实时推送都走 OpenIM。群里还有一个接了大模型的 agent，@ 它就能问。
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
**发布版把边车嵌在 `yptd` 里**，首次运行解到 `~/.yptd/bin/`，所以下载到的
是单个文件；开发树里不嵌，在同目录或 `PATH` 上找。

---

## 安装与登录

拿到邀请码之后，一条命令：

```sh
curl -fsSL https://im.zhanghuanyang.com/dl/install.sh | sh
```

装的是一个可执行文件（macOS，arm64 / x86_64 自动选），有 Developer ID 签名并已公证。
用 `curl` 下载不带 quarantine 属性，不会有"无法验证开发者"的拦截。

### 从源码装

需要 Rust 1.90+、Go 1.24+ 和 C 编译器（边车里的 sqlite 走 CGO）。

```sh
git clone https://github.com/his1devil/yptd-serve.git yptd-tui
cd yptd-tui
make install            # 构建 release 并装到 ~/.cargo/bin：yptd + yptd-sidecar
```

构建边车时如果报 `writing go.mod cache: ... permission denied`，是 Go 模块缓存的属主
不对——某次 `sudo go ...` 会把整个缓存写成 root 的，之后任何新模块都下不下来：

```sh
ls -ld $(go env GOMODCACHE)/cache/download/github.com/*   # 看有没有 root
sudo chown -R "$(id -un):$(id -gn)" "$(go env GOMODCACHE)"
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

### 终端选哪个

图片显示走终端的图形协议，差别很大：

| 终端 | 协议 | 效果 |
| --- | --- | --- |
| Ghostty / kitty / WezTerm | Kitty | 最好。图片数据只传一次，之后重绘只发摆放指令 |
| iTerm2 | iTerm2 inline | 清晰，但每次重绘都要重发整张 base64 PNG，滚动带图消息偏沉 |
| 系统"终端" / Alacritty | 无 | 退化成半块字符，图片是马赛克 |

状态栏右上角写着当前协商到的协议（`kitty 14×30`、`iterm2 …`、`halfblocks 10×20`）。

---

## 打包分发

```sh
make dist               # 两个架构各一个 tar.gz，边车签名后嵌入，再签 yptd
make notarize           # 送苹果公证
```

`make dist` 从钥匙串里取第一个 Developer ID Application 证书，也可以用
`YPTD_SIGN_ID` 指定。公证要先存一次凭据：

```sh
xcrun notarytool store-credentials yptd \
    --apple-id <你的 Apple ID> --team-id <团队 ID> --password <App 专用密码>
```

命令行二进制**没法 staple**（`stapler` 只认 .app / .dmg / .pkg），所以公证票据
留在苹果服务器上，Gatekeeper 首次运行时联网查一次。聊天客户端本来就要联网，够用；
要做到离线也零警告，得补一张 Developer ID Installer 证书、打成 `.pkg` 再 staple。

打好之后把 `dist/` 里的 `*.tar.gz`、`SHA256SUMS`、`install.sh` 传到服务器的
`/dl/` 下即可，`install.sh` 认 `uname -m` 自己挑包，下载后校验 sha256。

### 不进界面的自检

```sh
yptd doctor                       # 登录、起边车、拉会话与历史，监听 8 秒推送
yptd doctor "hello"               # 同上，并向最近的会话发一条
yptd doctor --frame 120x32        # 把真实数据渲染成一帧文本，ssh 里也能看
yptd doctor --new 群名 lina wuhao   # 建群并拉人，读回成员
yptd doctor --reply lina 文本       # 引用最新一条并 @ 这个人，打印回显
yptd doctor --img 图片路径          # 发一张图，再按别人的方式取回来解码
yptd doctor --media                 # 终端的图形能力，以及图片实际能拿到多少像素
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
| `browse` | 图片浏览器盖在会话上，勾了两张 |
| `attach` | 三张图待发，说明写了一半 |

---

## 键位

| 键 | 作用 |
| --- | --- |
| `1` `2` `3` | 聚焦 会话 / 消息 / 成员 |
| `Tab` `S-Tab`、`h` `l` | 切换焦点 |
| `j` `k`、`C-n` `C-p` | 移动**选择光标**，视口跟随 |
| `J` `K` | 视口上下一行 |
| `C-d` `C-u` | 半页滚动 |
| `g` `G` | 跳到顶部 / 最新（`G` 重新开启自动跟随） |
| `Enter` | 打开会话；在分类上则折叠 |
| `z` | 折叠 / 展开分类 |
| `r` | 引用选中的消息并进入 INSERT |
| `i` | 进入 INSERT（`Esc` 退出） |
| `@` | INSERT 里：弹出本会话成员名单，选中即插入 |
| `:` | INSERT 里、且在词首：弹出表情，按 shortcode 过滤 |
| `Enter` / `S-Enter` | INSERT 里：发送 / 换行 |
| `C-a` `C-e` `C-u` `C-k` `C-w`、`M-←` `M-→` | INSERT 里的行编辑 |
| `:` | 进入 COMMAND（`Esc` 退出） |
| `q` | 退出 |

鼠标：点击切换焦点与选中，双击会话打开，滚轮滚动指针所在的栏（不用先点它）。

消息流按**行**滚，不按条：滚轮一格三行，带图的长消息不会一下整条跳过去。视口和选择
光标互相拖着走——滚轮把光标滚出屏幕时光标跟到屏内，`j` `k` 把光标移出屏幕时视口跟到
光标；滚回底部自动恢复跟随最新。列表最顶上一行说明再往上会发生什么：
"↑ 继续上滚查看更早的消息"、"正在加载更早的消息…"、"── 这是最早的消息 ──"。
离顶部不到 30 行就提前取上一页，正常滚动时页是提前到的。

### 命令（`:` 进入，`Enter` 执行）

| 命令 | 作用 |
| --- | --- |
| `:new 群名` | 建群（自己是群主），打开后立刻弹出邀请名单 |
| `:invite` | 在当前群弹出邀请名单；`:invite lina wuhao` 直接拉这几个人 |
| `:dm` | 弹出名单选一个人私聊；`:dm lina` 直接开 |
| `:img` | 打开图片浏览器，可多选 |
| `:img 路径` | 直接附带一张图，`~` 会展开，相对路径按当前目录算 |
| `:help` | 命令一览 |
| `:q` | 退出 |

`Esc` 一次剥一层：先清掉引用、提及和待发的图片，再退出 INSERT。切换会话会一并丢掉——
它们属于刚才那个会话，带过去就会引用别人看不见的消息。

### 发图片

三条路，殊途同归：**把文件从访达拖进终端窗口**（可以一次拖多个），
**`:img` 打开浏览器挑**（`Space` 多选，`Backspace` 上一级），
或者 **`:img 路径`** 直接指定。

图片不会立刻发出去，而是挂在输入框上方等你，可以再补一句说明，`Enter` 一起发，
`Esc` 取消。上传在后台线程排队，界面不会卡住。

图片缩放用 Lanczos，不是库默认的最近邻——把两千像素宽的照片降到几百像素时，
最近邻等于每四个像素扔掉三个，出来就是糊的。清晰度还取决于终端报给我们的
单元格像素数，`yptd doctor --media` 会把这些数字打出来。

但 Lanczos 在两千多万像素上要跑将近半秒，而这活儿本来是在绘制线程上做的，
一个满是照片的会话打开时就会卡好几秒。所以下载线程里先用便宜的滤镜把长边缩到
1600，主线程拿到的就只有原来二十分之一的像素量。1600 远大于终端能显示的几百
像素，所以清晰度没有损失。

缩放本身也不再交给绘图库。库的做法是拿着整张图和一块区域，每次绘制时按它自己
选的滤镜缩一次；改成我们先把图缩到那块区域**确切的像素画布**，再建一个固定尺寸
的协议交给它，于是滤镜是我们选的，而且每张图每个尺寸只编码一次。编码在单独的
线程里做，绘制线程只负责贴上去，没准备好的那一格先空着。一排并列的图用「铺满后
居中裁切」，所以每格都是实打实占满的。解码后的图最多留 24 张，久没画的先扔，
滚回去时从磁盘缓存重新读。

还有一个和顺序有关的坑：向终端询问图形能力和单元格像素数，是在 stdin 上的一次
往返。**必须在读键盘的线程启动之前做**，否则终端的回答被那个线程当键盘输入吃掉，
探测超时，图片就退化成半格字符画，看起来又糊又卡。状态栏右侧的 `图 kitty 14×30`
就是探测结果，`kitty`/`iterm2`/`sixel` 之一加上单元格像素数才算正常；看到
`halfblocks` 或者很小的像素数，说明探测没成功。

同一个人连着发的图片，界面上合成一块**一排并列**显示，每张占同样大的格子，
不管原图是横是竖。超过四张只画前四张并标出还有几张。单张则保持原比例完整显示。
OpenIM 没有能装多张图的消息类型，所以发出去仍然是几条消息，合并只发生在显示上。

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
- **界面先起，连接在后台。** 登录、起边车、等同步在一个线程里做，界面立刻出来显示
  "连接中…"，好了整份快照换进来；连接失败则退出并把原因打在终端上。
- **登录后等同步，但不久等。** SDK 的 `Login` 在 socket 建好时就返回，会话与群是之后异步
  拉进本地库的；客户端等 `OnSyncServerFinish`（最多 3 秒，连接后静默 0.6 秒也算）再读列表。
  没等到的，`OnSyncServerFinish` 事件到了会触发重拉，不为它卡第一帧。
- **发出去的消息以 SDK 回显为准。** 不在本地猜 ID；`send` 返回的完整消息进快照。
- **历史按会话懒拉一次**（最新 60 条）并标记已读；同步完成后清掉缓存重新拉。
- **能登录就能用 bot。** 注册要邀请码，那一道就是门槛；再维护一份"谁可以用 bot"的白名单
  只是把同一道门写两遍。`yptd-server bot block <用户>` 停用个别人，`bot list` 看谁被停用了。
- **弹层独占键盘。** 名单打开时所有按键归它，`q` 是过滤字符而不是退出；鼠标点在下面的栏上不生效。
- **私聊会话本地先建行。** `:dm` 用 SDK 的规则算出 `si_<a>_<b>`（按字典序排的两个 id），
  服务端在第一条消息发出后才有这条会话，之后推来的行会落在同一个 id 上。
- **引用和 @ 是同一条消息。** SDK 把被引用的原文塞在 at-text 元素里，
  所以「回复并 @」发出去是一条 contentType 106，而不是两条；读的时候两个位置都要认。
- **发送前重新核对提及。** 名单里选过的人，如果名字又被删掉了，就不该收到通知；
  发送时按最终文本里还在不在 `@昵称` 过一遍，并按 id 去重。
- **图片按 URL 缓存，不按文件名。** 两个人各发一张 `screenshot.png` 是两张图，
  按名字缓存会把第一张显示两遍。下载走一个后台线程排队，结果和边车事件走同一个通道，
  界面从不为下载等待；缓存落在 `~/.yptd/cache/`，删掉会重新下。
- **终端不支持图形协议就不下载。** 反正也画不出来，省流量。

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
  yptd-tui/      三栏骨架、消息行渲染、双光标、图片下载显示与相册排布、
                 拖拽与文件浏览、离屏截帧、登录与会话、命令与弹层
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

## 群里的 agent

`agentbot`（昵称 HALX）是一个普通账号，把它拉进群，@ 它提问，或者直接私聊它。
背后是 [opencode](https://opencode.ai) 驱动的 GLM-5.3。

它**不跑 IM 客户端**：没有长连接、没有 token、不需要边车。OpenIM 在有人说话时
回调 yptd-server，服务端判断是不是在叫它，然后用管理接口以它的身份把答案发回去。
崩了也不丢消息，OpenIM 该投递还是投递。

- 每个会话（群或私聊）对应一条独立的 opencode 会话，所以在一个群里连着问几句是有
  上下文的，换个群是另一条线
- 答案超过 3 秒才会先发一条「⏳ 正在处理…」；这条消息带着 `ex` 标记，客户端在
  答案到达后自动把它藏起来
- 白名单默认是空的，也就是**谁都不能用**。这是故意的：背后是个会执行命令的 agent

管理：

```sh
yptd-server bot setup                # 建 agentbot 账号
yptd-server bot allow <userID>       # 放行一个人
yptd-server bot deny  <userID>
yptd-server bot list
yptd-server bot check                # opencode 通不通、账号在不在、白名单几个人
```

服务端跑着两个东西：`yptd-opencode.service`（opencode serve，专用低权限账号
`yptdbot`，systemd 把整个文件系统锁成只读，只有它自己的目录可写）和 `yptd-server`
里的回调路由。OpenIM 那侧在 `webhooks.yml` 打开了 `afterSendGroupMsg` 和
`afterSendSingleMsg`，只放行 101 和 106 两种消息类型。

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

收图后另存为、剪贴板里的截屏直接粘贴、发文件与视频、reaction、撤回、
重发失败的消息、退群 / 踢人、模糊会话切换器、leader 键提示窗、可配置键位。
凭据先放 0600 文件，钥匙串以后再说。

## 消息是怎么排出来的

排好的行会缓存，键是（会话、宽度、消息版本、图片版本、时区、当天、顶部提示状态）。
解析 markdown、语法高亮、按宽度换行都只在这几样真的变了时才做一次；移动光标和滚动视口
不触发重排，选中竖条是画的时候现加的。语法高亮的语法表在启动时就在后台线程预热，
免得第一个代码块出现时卡两三百毫秒。

视口的位置记成一个锚：（某条消息，离它正文首行几行）。日期分隔线、未读分隔线、
作者组之间的空行都不算在正文里，所以上一页历史插进来、分隔线挪了位，锚指的还是同
一行字。滚轮只是把"要滚几行"记下来，真正换算成锚的是渲染这一帧的时候——只有它知道
每条消息占几行。

历史分页在自己的线程里拉，主线程不等它；一页全是通知、一条都显示不出来时，从这页最老
那条的 clientMsgID 接着往前翻，而不是拿同一个起点反复要同一页。取失败 5 秒内不重试。
图片下载解码开 3 个线程，预缩放用 box filter（`thumbnail`），一张 4000 像素的照片从
236ms 到 148ms。

## 许可

MIT OR Apache-2.0
