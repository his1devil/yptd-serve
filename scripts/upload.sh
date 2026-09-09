#!/bin/sh
# 把 dist/ 里的东西发布到服务器。
#
# 只在公证通过之后跑——`make publish` 会按顺序串起来，中间失败就停，
# 免得把没验过的包发出去。
set -eu

cd "$(dirname "$0")/.."
HOST=${YPTD_DIST_HOST:-root@8.160.186.31}
DIR=${YPTD_DIST_DIR:-/var/www/yptd/dl}

for f in yptd-macos-arm64.tar.gz yptd-macos-x86_64.tar.gz SHA256SUMS install.sh VERSION; do
    [ -f "dist/$f" ] || { echo "dist/$f 不在，先 make dist" >&2; exit 1; }
done

scp -q dist/yptd-macos-arm64.tar.gz dist/yptd-macos-x86_64.tar.gz \
       dist/SHA256SUMS dist/install.sh dist/VERSION "$HOST:$DIR/"
ssh "$HOST" "chmod 644 $DIR/*"

echo "已发布 $(cat dist/VERSION) 到 $HOST:$DIR"
echo "线上版本：$(curl -fsS https://im.zhanghuanyang.com/dl/VERSION)"
