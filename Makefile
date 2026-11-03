BUDGET_SCALE ?= 3
TARGET ?= x86_64-unknown-linux-musl
SIZE_LIMIT := 10000000

.PHONY: check budget size upgrade

# The gate CI runs.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo machete
	cargo deny check
	cargo test
	BUDGET_SCALE=$(BUDGET_SCALE) cargo test --release --test budgets -- --ignored

# Timed budgets at 1x, for the reference host.
budget:
	BUDGET_SCALE=1 cargo test --release --test budgets -- --ignored --nocapture

size:
	cargo build --release --target $(TARGET)
	@bytes=$$(stat -c%s target/$(TARGET)/release/tachylite); \
	echo "release binary $(TARGET): $$bytes bytes (limit $(SIZE_LIMIT))"; \
	test $$bytes -le $(SIZE_LIMIT)

upgrade:
	rustup update stable
	cargo upgrade --incompatible
	cargo update
