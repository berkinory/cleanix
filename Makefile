.PHONY: build run test check
build:
	./cargo-local build --release --locked
run: build
	./target/release/cleanix
check:
	./cargo-local fmt --check
	./cargo-local clippy --all-targets -- -D warnings
test: check
	./cargo-local test --locked
