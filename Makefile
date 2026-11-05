BUDGET_SCALE ?= 3
TARGET ?= x86_64-unknown-linux-musl
SIZE_LIMIT := 10000000

.PHONY: check browser budget size upgrade mermaid

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

# The vendored Mermaid script. To upgrade, change both lines to the npm registry's
# version and `dist.integrity`, then run `make mermaid`.
MERMAID_VERSION := 12.3.0
MERMAID_SHA512 := 90BO2kQvmQWj9YjBX2IsLHCw61rN69dPTn5wFfDu+b9aT7qtzfzTA3wpKWeaRAKK85refSo9How5EOeY39DWfA==

mermaid:
	tmp=$$(mktemp -d) && \
	curl -fsS -o $$tmp/m.tgz https://registry.npmjs.org/@mermaid-js/tiny/-/tiny-$(MERMAID_VERSION).tgz && \
	test "$$(openssl dgst -sha512 -binary $$tmp/m.tgz | base64 -w0)" = "$(MERMAID_SHA512)" && \
	tar xzf $$tmp/m.tgz -C $$tmp package/dist/mermaid.tiny.js package/LICENSE && \
	mkdir -p src/vendor && \
	gzip -9nc $$tmp/package/dist/mermaid.tiny.js > src/vendor/mermaid.tiny.js.gz && \
	cp $$tmp/package/LICENSE src/vendor/MERMAID-LICENSE && \
	rm -rf $$tmp
