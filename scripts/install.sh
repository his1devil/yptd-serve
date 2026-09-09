#!/bin/sh
# yptd 安装脚本。
#
#   curl -fsSL https://im.zhanghuanyang.com/dl/install.sh | sh
#
# 装的是一个可执行文件，边车在里面，首次运行时自己解到 ~/.yptd/bin。
# 用 curl 下载的文件不带 quarantine 属性，加上二进制本身有 Developer ID
# 签名并已公证，所以不会有"无法验证开发者"那类拦截。
set -eu

BASE=${YPTD_BASE_URL:-https://im.zhanghuanyang.com/dl}
BIN_DIR=${YPTD_BIN_DIR:-$HOME/.local/bin}

case "$(uname -s)" in
    Darwin) ;;
    *)
        echo "目前只提供 macOS 的包。Linux 请从源码构建：" >&2
        echo "  git clone https://github.com/his1devil/yptd-serve && cd yptd-serve && make install" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    arm64)  arch=arm64 ;;
    x86_64) arch=x86_64 ;;
    *)      echo "不认识的架构 $(uname -m)" >&2; exit 1 ;;
esac

tarball="yptd-macos-$arch.tar.gz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "下载 ${tarball}…"
curl -fsSL "$BASE/$tarball" -o "$work/$tarball"
curl -fsSL "$BASE/SHA256SUMS" -o "$work/SHA256SUMS"

echo "校验…"
(cd "$work" && grep " $tarball\$" SHA256SUMS | shasum -a 256 -c -) >/dev/null || {
    echo "校验失败，没装。网络中间出了问题，或者服务器上的包和校验和对不上。" >&2
    exit 1
}

tar xzf "$work/$tarball" -C "$work"
mkdir -p "$BIN_DIR"
# 先搬到位再改名：正在运行的 yptd 不会被写坏。
mv "$work/yptd" "$BIN_DIR/.yptd.new"
chmod 755 "$BIN_DIR/.yptd.new"
mv "$BIN_DIR/.yptd.new" "$BIN_DIR/yptd"

# 早先的版本是两个文件，边车现在在 yptd 里面了。只清我们自己这个目录里的，
# 别人装在别处的那份不归这个脚本管。
if [ -f "$BIN_DIR/yptd-sidecar" ]; then
    rm -f "$BIN_DIR/yptd-sidecar"
    echo "清掉了旧版留下的 $BIN_DIR/yptd-sidecar"
fi

echo
echo "装好了：$BIN_DIR/yptd"

# 装对地方还不够，还得是敲 yptd 时真正跑起来的那个。
case ":$PATH:" in
    *":$BIN_DIR:"*)
        found=$(command -v yptd 2>/dev/null || true)
        if [ -n "$found" ] && [ "$found" != "$BIN_DIR/yptd" ]; then
            echo
            echo "注意：PATH 上还有一个更靠前的 ${found}，敲 yptd 跑的是它。"
            echo "删掉它，或者把 $BIN_DIR 挪到 PATH 前面。"
        fi
        ;;
    *)
        echo
        echo "$BIN_DIR 不在 PATH 上。把这行加进 ~/.zshrc："
        echo "  export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac

cat <<'EOF'

下一步：
  yptd login     用邀请码注册，跟着提示走
  yptd           进聊天界面

终端建议用 Ghostty、kitty 或 WezTerm，图片最清晰。
iTerm2 也能显示图片。系统自带的"终端"和 Alacritty 没有图形协议，
图片会退化成马赛克方块。
EOF
