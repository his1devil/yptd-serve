---
name: yptd-notes
description: "记录和查询 yptd 的 bug 与需求，用 yptd-note 命令读写文件。TRIGGER：有人说「记一下」「这是个 bug」「有个问题」「希望能…」「加个功能」「建议」；@Dummy 发来截图或录屏；问「有哪些没修的」「上周记了什么」「#0917-2031-a3 怎么样了」「这个修好了」。只通过 yptd-note 写，不要自己建文件、改文件。"
---

# yptd 记录本

所有记录是 `~/notes/` 下的 Markdown 文件，一条一个，按 年/月 归档，文件名 = 编号-标题。
读可以 `yptd-note list/show`，也可以直接 grep 文件。
**写只能用 `yptd-note`**——它负责编号、格式、附件下载和 git 提交。自己写文件会撞号、丢附件。

## 记一条

```bash
yptd-note add --kind bug --title "一句话标题" \
  --from "昵称 (userID)" --where "#群名 (groupID)" --msg <clientMsgID> \
  --attach <URL> --attach-name <原文件名> <<'EOF'
用户的原话，逐字，不要改写、不要翻译
EOF
```

- `--kind`：`bug` 现在的行为和预期不符 / `需求` 现在没有、希望有 / `待定` 说不清。**拿不准就 待定，不要猜。**
- `--title`：你起的，一句话，不超过 20 字，说清楚是什么，不写「关于…的问题」这种空话。
- `--from` / `--where`：从消息里来（谁说的、哪个群）。私聊没有 `--where`。
- `--attach`：消息附的每个文件一个，URL 原样给；`--attach-name` 按同样顺序给原文件名。
- 正文从标准输入读，用 heredoc。

命令**只输出一行编号**，如 `0917-2031-a3`。**回复里的编号必须是这一行。**
命令报错或没有输出 = 没记上，要直说「没记上：<错误>」，不要说「记下了」。

## 先查重

add 之前先看有没有同一件事：

```bash
yptd-note list --status open | grep -i 关键词
```

有相近的，不新建，补到那条：

```bash
yptd-note append <id> --from "昵称 (userID)" <<'EOF'
补充的原话
EOF
```

回复「和 #<id> 是一回事，补在那条里了」。

## 查

```bash
yptd-note list                         # 全部，新的在前
yptd-note list --status open           # 没处理的
yptd-note list --kind bug --since 7d   # 一周内的 bug
yptd-note list --from 阿森
yptd-note show <id>                    # 整条，含原话和附件
```

回答查询只列事实：编号、类型、谁提的、什么时候、状态、标题。不评价。

## 改状态

```bash
yptd-note set <id> status=done      # 明确说「修好了」「上线了」
yptd-note set <id> status=wontfix   # 明确说「不做了」「不改」
yptd-note set <id> kind=bug         # 一开始标错了
```

只有对方**明确说了**才改。「应该修好了吧」不算。

## 附件

`--attach` 给 URL 就行，命令会下载到 `assets/<id>/`；超过 200MB 或下载失败只记链接，记录照样成立。
如果你能看到图片内容（模型支持看图），在正文末尾另起一段：

```
## 截图里看到
一两句：什么界面、哪里不对
```

看不到就不写这段，不要编。
