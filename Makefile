# yptd：Rust 客户端 + Go 边车。边车必须和 yptd 同目录（或在 PATH / $YPTD_SIDECAR）。
#
#   make            构建两者到 target/debug/
#   make release    构建两者到 target/release/
#   make install    装到 ~/.cargo/bin/（yptd 与 yptd-sidecar）
#   make dist       打分发包：签名的单文件，边车嵌在里面
#   make notarize   把 dist/ 送去苹果公证
#   make publish    打包 → 公证 → 上传，中间失败就停
#   make test       Rust + Go 测试

PROFILE ?= debug
CARGO_FLAGS := $(if $(filter release,$(PROFILE)),--release,)
OUT := target/$(PROFILE)

.PHONY: all release sidecar tui install test clean dist notarize upload publish

all: tui sidecar

release:
	$(MAKE) PROFILE=release all

tui:
	cargo build $(CARGO_FLAGS)

# CGO：openim-sdk-core 用 mattn/go-sqlite3，需要本机有 C 编译器。
sidecar:
	cd sidecar && CGO_ENABLED=1 go build -o ../$(OUT)/yptd-sidecar ./cmd/yptd-sidecar

install: release
	install -d $(HOME)/.cargo/bin
	install -m 755 target/release/yptd target/release/yptd-sidecar $(HOME)/.cargo/bin/
	@echo "已安装到 ~/.cargo/bin：yptd, yptd-sidecar"

# 分发：签名 + 单文件。要 Developer ID 证书，见 scripts/dist.sh。
dist:
	./scripts/dist.sh

# 公证。要先跑一次 notarytool store-credentials，见 scripts/notarize.sh。
notarize:
	./scripts/notarize.sh

# 上传到服务器。单独跑要自己保证已经公证过；正常走 publish。
upload:
	./scripts/upload.sh

# 发布一版：打包 → 公证 → 上传。任一步失败就停，不会把没验过的包发出去。
publish:
	$(MAKE) dist
	$(MAKE) notarize
	$(MAKE) upload

test:
	cargo test
	cd sidecar && go test ./...
	cd server && go test ./...

clean:
	cargo clean
	rm -f target/*/yptd-sidecar
