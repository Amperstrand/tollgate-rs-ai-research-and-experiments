.PHONY: test test-v1 test-openwrt test-all lint clippy fmt build build-release build-ipk clean

test:
	cargo test --workspace

test-v1:
	cargo test --workspace --features v1-compat

test-openwrt:
	cargo test --workspace --features openwrt

test-all:
	cargo test --workspace
	cargo test --workspace --features v1-compat
	cargo test --workspace --features openwrt

lint: fmt clippy

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --all-targets --features v1-compat -- -D warnings
	cargo clippy --workspace --all-targets --features openwrt -- -D warnings

build:
	cargo build --workspace

build-release:
	cargo build --release --features v1-compat,spilman

build-ipk:
	bash packaging/openwrt-ipk/build-ipk.sh --arch aarch64

clean:
	cargo clean
