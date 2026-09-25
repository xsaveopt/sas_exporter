OUT     ?= ../bin
TARGETS  = x86_64-unknown-linux-musl aarch64-unknown-linux-musl

.PHONY: build test lint clean

build:
	cargo build --release --locked $(addprefix --target ,$(TARGETS))
	mkdir -p $(OUT)
	cp target/x86_64-unknown-linux-musl/release/sasctl $(OUT)/sasctl_linux_amd64
	cp target/aarch64-unknown-linux-musl/release/sasctl $(OUT)/sasctl_linux_arm64

test:
	cargo test --locked

lint:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings

clean:
	cargo clean
	rm -f $(OUT)/sasctl_linux_amd64 $(OUT)/sasctl_linux_arm64
