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
if ! xcrun notarytool submit dist/notarize.zip --keychain-profile "$PROFILE" --wait \
    | tee /tmp/yptd-notary.log; then
    echo "提交失败" >&2
    exit 1
fi
# --wait 在苹果判定 Invalid 时也可能以 0 退出，所以自己看结论。
if ! grep -q "status: Accepted" /tmp/yptd-notary.log; then
    echo "公证没有通过，看上面的日志" >&2
    exit 1
fi

echo
# 真正说明问题的检查：给副本打上 com.apple.quarantine 再运行。微信、浏览器、
# AirDrop 传过来的文件都带这个属性；公证之前这一步会被 Gatekeeper 直接 SIGKILL。
#
# 不用 codesign --test-requirement="=notarized" 做判定：裸二进制的票据不装订在
# 文件里，要等 Gatekeeper 联网取回并缓存之后那个检查才会过，所以刚公证完必然
# 报"没有票据"。下面这一跑本身就会让它取回。
probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT
cp dist/bin/yptd-arm64 "$probe/yptd"
xattr -w com.apple.quarantine "0083;00000000;probe;" "$probe/yptd"
if "$probe/yptd" --help >/dev/null 2>&1; then
    echo "带 quarantine 也能跑：微信或浏览器传过去不会被拦"
else
    echo "带 quarantine 时跑不起来，公证没生效" >&2
    exit 1
fi

# 票据这时应该已经缓存下来了，作为佐证打印，但不作判定。
printf "公证票据："
if codesign --test-requirement="=notarized" --verify dist/bin/yptd-arm64 2>/dev/null; then
    echo "已缓存"
else
    echo "还没缓存到本机（不影响分发，Gatekeeper 会在用户那边联网取）"
fi
