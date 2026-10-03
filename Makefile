.PHONY: all build build-client build-client-e2e build-server test test-rust test-e2e serve serve-http clean

all: build test

build: build-client build-server

build-client:
	@echo "==> Building WebAssembly client with Trunk..."
	cd crates/client && trunk build index.html --release

build-client-e2e:
	@echo "==> Building WebAssembly client with E2E test hooks (dist-e2e, never deployed)..."
	cd crates/client && trunk build index.html --release --features e2e-hooks --dist dist-e2e

build-server:
	@echo "==> Building Axum server..."
	cargo build -p server --release

test-rust:
	@echo "==> Running Rust unit and integration tests..."
	cargo test --workspace

test-e2e: build-client-e2e
	@echo "==> Running Playwright multi-peer browser E2E tests..."
	cd e2e && npm test

test: test-rust test-e2e

serve: build-client
	@echo "==> Starting dchat server in HTTPS mode (default port 8443)..."
	cargo run -p server --release -- --port 8443 --static-dir crates/client/dist

serve-http: build-client
	@echo "==> Starting dchat server in HTTP mode (port 3000)..."
	cargo run -p server --release -- --http --port 3000 --static-dir crates/client/dist

clean:
	cargo clean
	rm -rf crates/client/dist crates/client/dist-e2e
