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
# 不要用 spctl：它评估的是 .app，裸的命令行二进制永远报
# "does not seem to be an app"，看着像失败其实什么都没说明。
# codesign 的 =notarized 要求才是命令行二进制该用的检查。
echo "核对公证票据："
failed=0
for binary in dist/bin/yptd-*; do
    if codesign --test-requirement="=notarized" --verify "$binary" 2>/dev/null; then
        echo "  $(basename "$binary")  已公证"
    else
        echo "  $(basename "$binary")  没有公证票据"
        failed=1
    fi
done

# 真正决定朋友能不能跑起来的是这个：带 quarantine 属性时会不会被 Gatekeeper
# 杀掉。微信、浏览器、AirDrop 传过来的文件都带这个属性；公证之前这一步是
# 直接 SIGKILL。
probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT
cp dist/bin/yptd-arm64 "$probe/yptd"
xattr -w com.apple.quarantine "0083;00000000;probe;" "$probe/yptd"
if "$probe/yptd" --help >/dev/null 2>&1; then
    echo "  带 quarantine 也能跑，微信或浏览器传过去不会被拦"
else
    echo "  带 quarantine 时跑不起来，公证没生效" >&2
    failed=1
fi

exit "$failed"
