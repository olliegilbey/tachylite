BUDGET_SCALE ?= 3
TARGET ?= x86_64-unknown-linux-musl
SIZE_LIMIT := 10000000

.PHONY: check browser budget size upgrade

# The gate CI runs.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo machete
	cargo deny check
	cargo test
	BUDGET_SCALE=$(BUDGET_SCALE) cargo test --release --test budgets -- --ignored
	$(MAKE) browser

# The browser seam: Playwright drives headless Chromium against a real server.
browser:
	cargo build --release
	cd browser && npm ci && npx playwright install --only-shell chromium && \
	BUDGET_SCALE=$(BUDGET_SCALE) npx playwright test

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
