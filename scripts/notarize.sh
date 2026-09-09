#!/bin/sh
# 把 dist/ 里已签名的二进制送去公证。
#
# 一次性准备（需要 App 专用密码，去 appleid.apple.com 生成）：
#   xcrun notarytool store-credentials yptd \
#       --apple-id <你的 Apple ID> --team-id M7ZSWL69E9 --password <App 专用密码>
#
# 命令行二进制没法 staple（stapler 只认 .app/.dmg/.pkg），所以公证票据留在
# 苹果服务器上，Gatekeeper 首次运行时联网查。聊天客户端本来就要联网，够用。
set -eu

cd "$(dirname "$0")/.."
PROFILE=${YPTD_NOTARY_PROFILE:-yptd}

[ -d dist/bin ] || { echo "先跑 make dist" >&2; exit 1; }

echo "打包送审…"
rm -f dist/notarize.zip
ditto -c -k dist/bin dist/notarize.zip

echo "提交公证（要等几分钟）…"
xcrun notarytool submit dist/notarize.zip --keychain-profile "$PROFILE" --wait

echo
echo "核对 Gatekeeper 怎么看这些文件："
for binary in dist/bin/yptd-*; do
    printf '  %s: ' "$(basename "$binary")"
    spctl -a -vv -t exec "$binary" 2>&1 | tail -1
done
