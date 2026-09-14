# yptd 客户端接入文档

写给要做 iOS / Apple Watch 端的人。桌面端（Electron）已经把这套流程跑通了，
下面每一条都是从运行中的系统和代码里核对出来的，不是从记忆里写的；有拿不准的地方
标了「未验证」。

---

## 1. 两层，分别管什么

yptd 是两个服务拼起来的，搞混这一点会浪费很多时间：

| | 管什么 | 客户端怎么用 |
|---|---|---|
| **yptd-server** | 账号、邀请码、设备凭据、花名册、agent 名单、运行记录 | 自己写 HTTP 调用 |
| **OpenIM** | 会话、消息、群、已读、离线推送 | 用官方 iOS SDK |

关键点：**yptd-server 不转发消息**。它只负责把你换成一个 OpenIM 能认的身份，
之后收发消息完全是客户端和 OpenIM 之间的事。建群、拉人也是客户端直连 OpenIM
（这条在第 8 节会变得重要）。

---

## 2. 地址

线上这一套由 nginx 分流，都在 `im.zhanghuanyang.com` 下面：

| 用途 | 地址 |
|---|---|
| yptd-server | `https://im.zhanghuanyang.com/yptd` |
| OpenIM API | `https://im.zhanghuanyang.com` |
| OpenIM WebSocket | `wss://im.zhanghuanyang.com/ws` |
| OpenIM 对象存储（图片、文件） | `https://im-file.zhanghuanyang.com` |

注意 yptd-server 在 `/yptd` 前缀下，OpenIM 在根路径下，两者同域。
下文写 `POST /v1/login` 指的是 `https://im.zhanghuanyang.com/yptd/v1/login`。

---

## 3. 登录：三个 token，别弄混

```
邀请码 ──register──► device_token（长期，进钥匙串）
                     │
                     ├──login──► im_token（短期，给 OpenIM SDK）
                     │
                     └── 之后所有 yptd-server 请求的 Bearer
```

- **device_token**：长期设备凭据，**只在注册时返回一次**。存钥匙串，丢了要重新走邀请码。
  yptd-server 的所有接口都用它做 `Authorization: Bearer <device_token>`。
- **im_token**：OpenIM 的登录票，每次 `POST /v1/login` 换一张，交给 SDK 的 `login()`。
  会过期，过期后重新调 `/v1/login` 换新的。

### 3.1 注册（第一次）

```http
POST /v1/register
{ "invite_code": "ABCD-1234", "nickname": "张三",
  "user_id": "zhangsan",        // 可选，不给就从昵称推
  "password": "...",            // 可选，设了才能用密码登录
  "device_name": "iPhone 15",
  "platform_id": 1 }
```

返回 `{ user_id, device_token, im_token, nickname }`。

失败码都是可以直接展示给人看的：`invalid_invite` / `unknown_invite` / `invite_used` /
`invite_expired` / `user_exists` / `invalid_user_id` / `weak_password`。

> 用户名撞车（`user_exists`）**不会消耗邀请码**，服务端先查重再核销。可以让用户改个名重试。

### 3.2 之后每次启动

```http
POST /v1/login
{ "device_token": "<钥匙串里那个>", "platform_id": 1 }
```

返回 `{ user_id, im_token, nickname }`（不再返回 device_token）。

### 3.3 密码登录（换设备、凭据丢了）

```http
POST /v1/login/password
{ "user_id": "zhangsan", "password": "...", "device_name": "iPhone", "platform_id": 1 }
```

返回里**包含新的 device_token**，存起来替换旧的。

### 3.4 platform_id

OpenIM 的平台枚举，注册和登录都要带，它决定多端在线和推送的行为：

| 平台 | ID |
|---|---|
| iOS | 1 |
| Android | 2 |
| Windows | 3 |
| macOS | 4 |
| Web | 5 |
| Linux | 7 |
| iPad | 9 |

桌面端用的是 4。**iOS 用 1，iPad 用 9**。Apple Watch 没有独立枚举——它要么复用
iPhone 的会话（通过 WatchConnectivity 让 iPhone 代收代发），要么自己当作 iOS(1)
登录、占掉 iPhone 的在线位。后者会让 iPhone 被踢下线，**建议走前者**。

---

## 4. yptd-server 接口全表

所有接口都要 `Authorization: Bearer <device_token>`，除了注册、登录和 `/v1/invites/check`。

| 方法 | 路径 | 干什么 |
|---|---|---|
| POST | `/v1/register` | 用邀请码注册 |
| POST | `/v1/login` | 设备凭据换 im_token |
| POST | `/v1/login/password` | 密码登录，换新设备凭据 |
| GET | `/v1/me` | 自己的资料和开关 |
| PATCH | `/v1/me` | 改昵称（`{"nickname":"…"}`，1–32 字） |
| PUT | `/v1/me/password` | 设置/修改密码 |
| PUT | `/v1/me/privacy` | 两个隐私开关，见第 8 节 |
| GET | `/v1/users` | 花名册（**已经过滤**，见第 8 节） |
| GET | `/v1/users/{id}` | 按完整 ID 查一个人 |
| GET | `/v1/agents` | agent 名单和自我介绍 |
| GET | `/v1/invites` | 我发出去的邀请码 |
| POST | `/v1/invites` | 生成邀请码 |
| POST | `/v1/invites/check` | 注册前校验邀请码（不需要鉴权） |
| GET | `/v1/runs` | 最近的 agent 运行记录 |
| GET | `/v1/runs/{id}` | 单条运行记录 |
| GET | `/v1/runs/{id}/events` | SSE，实时看 agent 干活 |
| POST | `/v1/runs/{id}/cancel` | 中止一次运行 |
| GET | `/healthz` | 健康检查 |

错误统一是 `{"error":"错误码","message":"给人看的中文"}` 配合 HTTP 状态码。
**优先展示 `message`**，它是写给终端用户的。

### 4.1 `GET /v1/me`

```json
{ "user_id": "zhangsan", "nickname": "张三", "disabled": false,
  "has_password": true, "discoverable": false, "joinable": false }
```

### 4.2 `GET /v1/users`

```json
{ "users": [
  { "user_id": "zhangsan", "nickname": "张三", "joinable": false },
  { "user_id": "agentjomo", "nickname": "JOMO", "is_agent": true,
    "tag": "热点", "color": "#E8A33D", "joinable": true }
] }
```

`is_agent` 为真的是机器人，`tag` 是名字旁边那个小标签，`color` 是它的身份色
（一组为深色模式调过的值，浅色模式下要压暗一档再用）。

### 4.3 `GET /v1/agents`

比 `/v1/users` 多一个 `description`——agent 的自我介绍，从 opencode 那边读的，
适合放在「和它的对话从这里开始」那种空状态里。

---

## 5. OpenIM iOS SDK

服务端是 **OpenIM v3.8**（部署的是 release-v3.8 分支，桌面端用的 SDK 是
`3.8.3-patch.15.1`）。iOS 端要用**同一条 3.8 线**的官方 SDK —— 大版本对不上时
协议字段会对不齐，这是最容易浪费一整天的地方。具体的 pod/SPM 包名以
[openimsdk 的 iOS 仓库](https://github.com/openimsdk) 当前 3.8 分支的
README 为准，我没有在这台机器上验证过它，不在这里写一个可能过时的名字。

调用顺序照搬桌面端就行：

```
initSDK(apiAddr: "https://im.zhanghuanyang.com",
        wsAddr:  "wss://im.zhanghuanyang.com/ws",
        platformID: 1,
        dataDir: <沙盒里一个目录>)
  ↓
login(userID: <yptd 给的 user_id>, token: <im_token>)
  ↓
挂事件监听 → 拉会话列表 → 拉消息
```

**几个从桌面端踩出来的注意事项：**

- `initSDK` 是有状态的，**重复调会抛**。重连或热更之后先问 `getLoginStatus()`，
  已经是 LoggedIn 就别再 init/login 一遍。
- 刚登录时 `getGroupMemberList` 会先返回一张**空表**。不要把空表当成「这个群没人」
  存下来，否则后面的存在性判断会跳过重拉，这个群的成员再也不出现。等
  `onSyncServerFinish` 之后再要一次。
- 群成员超过 200 要翻页，只取 offset=0/count=200 的话大群名单是缺的。
- 会话列表和消息的顺序、已读、未读数都交给 SDK，别自己维护一份。

---

## 6. 我们自己的约定

这一节是 OpenIM 之外、yptd 自己定的东西。**不实现这些，消息能收发，但显示会不对。**

### 6.1 会话 ID

```
群：     sg_<groupID>
单聊：   si_<两个 userID 按字典序排序后用下划线连起来>
```

例：`tuitest` 和 `agentbot` 的单聊是 `si_agentbot_tuitest`（a 在 t 前面）。
**必须排序**，否则两端算出来的 ID 不一样，会话对不上。

### 6.2 ContentType

| 值 | 是什么 |
|---|---|
| 101 | 纯文本 |
| 102 | 图片 |
| 105 | 文件 |
| 106 | 带 @ 的文本 |
| 110 | 自定义（我们只用它做表情回应） |
| 114 | 引用回复 |
| 117 / 118 | 富文本 / markdown（服务端会发，SDK 的枚举里没有，**按数字比**） |
| 1000–5000 | 各种通知，**客户端应该整段忽略**，它们不是聊天内容 |

### 6.3 `ex` 字段：附件、运行卡、占位

消息的 `ex` 是一个 JSON 字符串，我们往里塞了三种东西。**不认识就忽略，不会出错。**

**附件**（文字和附件是同一条消息）：

```json
{"yptd":"rich",
 "a":[{"k":"i","u":"<url>","n":"照片.jpg","s":123456,"w":1600,"h":1200},
      {"k":"f","u":"<url>","n":"报告.pdf","s":98765}],
 "t":1}
```

- `k`：`i` 图片 / `f` 文件；`u` 地址；`n` 文件名；`s` 字节数；`w`/`h` 图片原始尺寸
- `t=0` 表示**发的人没打字**，正文是给不认识 `ex` 的端看的占位（`[图片]`）。
  认识 `ex` 的端应该把正文藏掉，只显示附件。

**agent 运行**：

```json
{"yptd":"pending","run":"run_xxx"}   // 占位气泡，agent 正在想
{"yptd":"run","run":"run_xxx"}       // 最终回答，带上是哪次运行
```

`pending` 的消息是**临时的**，agent 答完会被最终答案顶掉，不要把它存进本地历史。
拿 `run` 去 `/v1/runs/{id}/events` 可以看它干活的过程。

### 6.4 表情回应

回应不是 OpenIM 的原生能力，我们用自定义消息（110）实现：

```json
{"yptd":"reaction","target":"<被回应消息的 clientMsgID>","emoji":"👍"}
```

客户端要做三件事：
1. 把这类消息**从消息流里拿掉**（它不是一条消息）
2. 折叠成目标消息下面的一排表情
3. **同一个人对同一条消息发第二次 = 取消**（它是开关，不是计数器）

> 翻历史时同一条回应会被反复送来，折叠逻辑要记住已经处理过哪些，
> 否则表情数会随着翻页一直涨。

### 6.5 @ 提及

- 谁被 @ 了看 `atTextElem.atUserList`（**是 userID 列表，不是昵称**），
  「这条有没有 @ 我」只看它，不要去正文里匹配文字。
- `@全体成员` 在 `atUserList` 里是特殊值 `AtAllTag`，从 SDK 的 `getAtAllTag()` 拿。
- 正文里的 `@名字` 怎么上色是**显示层的事**，桌面端的规则是：
  在这个会话里的成员正常高亮（agent 橙、人蓝），是这个服务器的用户但不在这个会话里
  显示成灰色（@ 了也没人收得到，不该看着像通了），其余当普通文字。

### 6.6 正文的 markdown

桌面端只渲染：**围栏代码块**、**表格**、以及行内的 `**粗体**` / `` `代码` `` /
`~~删除~~` / `[文字](链接)` / 裸链接。列表和标题保留字面字符——它们会把 agent
分段回答的空行折掉。移动端可以自己取舍，但**表格一定要渲染**，agent 一天发好几张，
原样显示是读不了的。

---

## 7. agent 怎么用

- **群里**：消息里 @ 它就是派活（`atUserList` 里带上它的 userID）。
- **单聊**：给它发消息就是派活，不用 @。
- agent 的 userID 在 `/v1/agents` 里拿，`is_agent` 也会在 `/v1/users` 里标出来。
- 派完活之后，服务端会先发一条 `{"yptd":"pending","run":"…"}` 的占位消息，
  答完再发最终答案。想显示「正在思考 / 用了什么工具」就连 SSE：

```http
GET /v1/runs/{id}/events
Accept: text/event-stream
```

先推一条 `event: snapshot`（到目前为止的全部状态），之后是增量。
`snapshot` 的 `seq` 是「已经折进这个快照的最后一个事件序号」，断线重连后
只需要它之后的事件。

运行记录的结构见 `server/internal/run`：`status`、`thinking`、`text`、
`tools[]`、`usage`、`error`、`placeholder_msg_id`、`final_msg_id`。

---

## 8. 隐私开关（2026-09 新增，对客户端有影响）

每个账号有两个开关，**默认都是关的**：

| 开关 | 关着的时候 |
|---|---|
| `discoverable` | 不出现在别人的 `/v1/users` 名册里 |
| `joinable` | **不能被拉进群**，服务端会拒绝整个操作 |

`PUT /v1/me/privacy`，两个字段都可选，只传要改的那个：

```http
PUT /v1/me/privacy
{ "joinable": true }
```

### 关掉搜索不等于失联

`/v1/users` 关掉之后仍然看得见的人：自己、所有 agent、开了开关的人、
以及**邀请关系两端**（你邀请进来的人、邀请你进来的人）。

另外两条路不受开关影响，客户端应该用上：

- **群成员表来自 OpenIM，不经过 `/v1/users`**。已经和你同群的人照常看得见。
  桌面端因此把「名册」和「所有已打开群的成员」合起来当作「你认识的人」，
  否则关了搜索就等于对同事失联。
- **`GET /v1/users/{id}` 按完整 ID 查，忽略 discoverable**。这是找一个还没和你
  同群的人的唯一办法。只接受完整 ID，不接受前缀和昵称。

### joinable 关着时，拉人会整批失败

这条**必须在客户端处理**，否则用户会撞上一个莫名其妙的失败：

> OpenIM 的回调响应里有 `RefusedMembersAccount` 字段，但上游处理它的代码是
> **注释掉的**。所以一次拉 5 个人、其中 1 个不允许，**5 个全部失败**。

客户端要做的：`/v1/users` 返回的每个人都带 `joinable`，**选人界面把 `joinable:false`
的人画成不可选**，不要等用户点了发现整批失败。桌面端的做法是画成灰色、沉到列表底部、
带一句「未开放」。服务端那道拦截是防改客户端的兜底，不是给正常流程用的。

被拒绝时 OpenIM 返回的 `errMsg` 会说清是谁挡的，但**文案会被层层包装重复三遍**
（形如 `10001 10001 张三 没有开放被加入群聊 10001 张三 没有开放被加入群聊`），
显示前要收拾一下：去掉重复的错误码前缀，再把整句重复的那一半去掉。

---

## 9. 离线推送（APNs）

**现状：没配过。** 服务器上的 `config/openim-push.yml` 和上游默认值一字未改
（`git status` 干净），里面的 geTui / jpns 凭据都是 OpenIM 自带的示例值。
目前的表现就是：app 不在前台时收不到任何推送。

### 关键事实：OpenIM 不直连 APNs

这一点会决定你的方案。OpenIM 的离线推送只支持三个中转服务商：

```yaml
enable: geTui     # 或 fcm / jpns
```

- **geTui（个推）** — 国内常用，当前配置文件里选的就是它
- **jpns（极光 JPush）** — 国内常用
- **fcm（Firebase）** — iOS 上 FCM 也是转 APNs，但国内不通

也就是说 **APNs 证书要传到中转服务商的后台**，不是填进 OpenIM。链路是：

```
OpenIM ──HTTP──► 个推/极光 ──► APNs ──► iPhone
```

### 要做的事

1. 在个推或极光开一个应用，拿到 AppKey / MasterSecret。
2. 在 Apple Developer 后台生成 APNs 鉴权密钥（`.p8`，比证书省事，不会过期），
   连同 Key ID、Team ID、Bundle ID 一起传到服务商后台。
3. 把 AppKey / MasterSecret 填进 `config/openim-push.yml` 对应的段，重启 OpenIM。
4. iOS 端集成服务商的 SDK，把它给的 push token 通过 OpenIM SDK 上报
   （`setAppBadge` / 各家 SDK 的注册接口，具体看服务商文档）。
5. `iosPush` 那一段现在是默认值，上线前要改：

```yaml
iosPush:
  pushSound: xxx        # 占位符，改成真的声音文件名
  badgeCount: true      # 角标，保持开
  production: false     # 上 App Store 前必须改成 true，否则走沙盒 APNs
```

`production: false` 这条最容易忘——开发时是对的，上架后不改会导致所有推送静默失败。

### Watch 的推送

Apple Watch 的通知是 iPhone 转发过去的，不需要单独接推送服务商。
只要 iPhone 端收到了，Watch 自然会显示；要定制 Watch 上的样子做
Notification Controller 就行。

---

## 10. 容易踩的坑

按踩到的顺序，都是真实发生过的：

- **重复 `initSDK` 会抛。** 重连前先 `getLoginStatus()`。
- **刚登录时成员表是空的。** 见 5 节，别把空表当结论存下来。
- **通知类消息（1000–5000）要整段忽略**，否则消息流里会混进一堆系统事件。
- **单聊会话 ID 必须把两个 userID 排序**，不排序两端算出来的不一样。
- **回应是开关不是计数器**，翻历史会重复送，折叠时要去重。
- **`atUserList` 是 userID 不是昵称**，别去正文里匹配文字判断有没有 @ 我。
- **拉人是整批成败**，见 8 节，选人界面要先过滤。
- **OpenIM 的错误文案会被重复包装三遍**，直接显示很难看。
- **`production: false`** 上架前要改，见 9 节。

---

## 附：本地跑一遍

想在不动线上的前提下试，服务端有这些命令（在服务器上）：

```bash
yptd-server invite new          # 生成一个邀请码
yptd-server bot setup           # 把 agents.json 里的 agent 注册进 OpenIM
```

桌面端的实现可以直接对照：
- 登录与凭据：`yptd-desktop/src/renderer/src/im/auth.ts`
- SDK 封装：`yptd-desktop/src/renderer/src/im/client.ts`
- 上面第 6 节那些约定：`yptd-desktop/src/renderer/src/im/translate.ts`
