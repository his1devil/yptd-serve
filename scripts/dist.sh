#!/bin/sh
# 打出可分发的客户端：每个架构一个 tar.gz，里面只有一个可执行文件。
#
# 边车先签名再嵌进 yptd，最后签 yptd —— 解出来的边车带着自己那份签名，
# 而且是客户端自己写的文件，不带 quarantine，Gatekeeper 不会拦。
#
# 用 YPTD_SIGN_ID 指定签名身份，不给就取钥匙串里第一个 Developer ID Application。
set -eu

cd "$(dirname "$0")/.."
DIST=dist
VERSION=${VERSION:-$(git describe --tags --always 2>/dev/null || echo dev)}
SIGN_ID=${YPTD_SIGN_ID:-$(security find-identity -v -p codesigning |
    awk -F'"' '/Developer ID Application/ { print $2; exit }')}

if [ -z "$SIGN_ID" ]; then
    echo "找不到 Developer ID Application 证书。用 YPTD_SIGN_ID 指定，或先在开发者后台建一个。" >&2
    exit 1
fi
echo "版本 $VERSION"
echo "签名 $SIGN_ID"

rm -rf "$DIST"
mkdir -p "$DIST/bin"

for arch in arm64 x86_64; do
    case $arch in
        arm64)  rust_target=aarch64-apple-darwin; go_arch=arm64 ;;
        x86_64) rust_target=x86_64-apple-darwin;  go_arch=amd64 ;;
    esac

    echo
    echo "── $arch ── 边车"
    (cd sidecar && CGO_ENABLED=1 GOOS=darwin GOARCH=$go_arch \
        CGO_CFLAGS="-arch $arch" CGO_LDFLAGS="-arch $arch" \
        go build -trimpath -ldflags "-s -w" -o "../$DIST/bin/yptd-sidecar-$arch" ./cmd/yptd-sidecar)
    codesign --force --timestamp --options runtime \
        --sign "$SIGN_ID" "$DIST/bin/yptd-sidecar-$arch"

    echo "── $arch ── 客户端（嵌入已签名的边车）"
    YPTD_EMBED_SIDECAR="$PWD/$DIST/bin/yptd-sidecar-$arch" \
        cargo build --release --target "$rust_target" -p yptd-tui
    cp "target/$rust_target/release/yptd" "$DIST/bin/yptd-$arch"
    codesign --force --timestamp --options runtime \
        --sign "$SIGN_ID" "$DIST/bin/yptd-$arch"
    codesign --verify --strict "$DIST/bin/yptd-$arch"

    # 包里就叫 yptd，解开就能用。
    stage="$DIST/stage-$arch"
    mkdir -p "$stage"
    cp "$DIST/bin/yptd-$arch" "$stage/yptd"
    tar czf "$DIST/yptd-macos-$arch.tar.gz" -C "$stage" yptd
    rm -rf "$stage"
done

cp scripts/install.sh "$DIST/install.sh"
(cd "$DIST" && shasum -a 256 ./*.tar.gz | sed 's|\./||' > SHA256SUMS)

echo
echo "打好了："
ls -lh "$DIST"/*.tar.gz "$DIST"/SHA256SUMS "$DIST"/install.sh | awk '{ print "  " $9, $5 }'
echo
echo "下一步：make notarize（公证），然后把 dist/ 传到服务器。"
