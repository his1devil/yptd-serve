# yptd：Rust 客户端 + Go 边车。边车必须和 yptd 同目录（或在 PATH / $YPTD_SIDECAR）。
#
#   make            构建两者到 target/debug/
#   make release    构建两者到 target/release/
#   make install    装到 ~/.cargo/bin/（yptd 与 yptd-sidecar）
#   make test       Rust + Go 测试

PROFILE ?= debug
CARGO_FLAGS := $(if $(filter release,$(PROFILE)),--release,)
OUT := target/$(PROFILE)

.PHONY: all release sidecar tui install test clean

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

test:
	cargo test
	cd sidecar && go test ./...
	cd server && go test ./...

clean:
	cargo clean
	rm -f target/*/yptd-sidecar
