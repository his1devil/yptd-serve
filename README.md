# yptd-tui

对接 OpenIM 的终端 IM 客户端，Rust + [ratatui](https://ratatui.rs)。

交互与视觉设计参考 [concord](https://github.com/chojs23/concord)（Discord 的 TUI 客户端），
但代码从零编写，不含其 GPL 代码。

> **当前状态：渲染层骨架。** 数据来自 `im-model::mock` 的固定快照，
> OpenIM 边车尚未接入，因此不需要任何服务端或账号就能跑起来看。

---

## 快速查看

只需要 Rust 1.90+：

```sh
# 没装 Rust 的话
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

git clone https://github.com/his1devil/yptd-serve.git yptd-tui
cd yptd-tui
cargo run
```

`q` 退出。首次编译约 30 秒。

### 不进交互也能看

三种离屏输出，方便截图、比对和回归：

```sh
# 纯文本，管道友好
cargo run -- --snapshot 112x28 top

# 真 ANSI —— 用你自己终端的配色，这才是真实观感
cargo run -- --ansi 112x28 top

# 单文件 HTML，逐格定宽，可直接用浏览器打开或截图
cargo run -- --html 112x28 top dark > frame.html
```

参数：`--<模式> <宽>x<高> [场景] [配色]`

| 场景 | 看什么 |
| --- | --- |
| `live` | 默认。跟随最新消息，含附件、发送失败、ghost 回复 |
| `top` | 会话顶部。日期分隔线、系统通知、提及高亮、reaction、已读回执 |
| `code` | 围栏代码块与行内代码 |
| `nav` | 焦点在会话栏，一个分类被折叠 |
| `insert` | 输入模式，composer 激活 |

配色只影响 `--html`（`dark` / `light`）。终端里跑时用你自己的配色。

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
| `i` | 进入 INSERT（`Esc` 退出） |
| `:` | 进入 COMMAND（`Esc` 退出） |
| `q` | 退出 |

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

## 测试

```sh
cargo test          # 97 个
cargo build         # 应当零警告
```

值得一看的几条，它们守着容易写错的地方：

- `selecting_a_message_does_not_change_how_many_lines_it_occupies`
  —— 选中态只加左侧竖条、不改内容宽度。否则每次移动光标都要重排换行，视口就守不住了。
- `wrapping_never_loses_or_duplicates_text` —— 4..40 列全宽度往返一致。
- `cjk_is_measured_two_cells_wide` —— 中文按显示宽度而非字符数换行。
- `spans_survive_a_rewrite_that_lengthens_the_text` —— 文本改写后标注跟着平移，不重新解析。
- `scrolling_to_a_message_keeps_its_own_divider_in_view` —— 分隔线属于它引出的那条消息。

---

## 结构

```
crates/
  tui-theme/     具名高亮组 + link 继承；颜色与边框几何分离
  tui-richtext/  纯文本 + 字节区间标注；改写重映射、宽度感知换行、Markdown 子集
  im-model/      OpenIM 语义的领域模型 + 合成雪花 ID + mock 快照
  yptd-tui/      三栏骨架、消息行渲染、双光标、离屏截帧
```

依赖方向单向：`yptd-tui` → `im-model` / `tui-richtext` / `tui-theme`，反向不可见。

### 两个设计要点

**主题只认名字，不认颜色。** 渲染代码写 `theme.style(HG::MessageAuthor)`，
从不写 `Color::Cyan`。90 个高亮组之间可 `link` 继承——改一个 `Muted`，
所有次要文本一起变。默认配色全部用 16 个 ANSI 名，所以客户端跟随你终端已有的配色方案，
而不是自带一套跟你抢。

**消息 ID 自带时间。** OpenIM 用 `clientMsgID`(UUID) + 每会话 `seq` 标识消息，
两者都不能跨会话排序。把发送时间打进高 42 位，一次拿到三样东西：
与真实时间一致的全序、不必单独存储的时间戳、以及便宜到能当 `BTreeMap` 键的 `Copy` 类型。
低 22 位是毫秒内计数器。

---

## 尚未实现

模糊会话切换器、leader 键提示窗、弹层、滚动条、syntect 语法高亮、
终端内图片（Kitty / iTerm2 / Sixel）、可配置键位、OpenIM 边车。

## 许可

MIT OR Apache-2.0
