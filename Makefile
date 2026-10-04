.PHONY: all build build-client build-client-e2e build-server test test-rust test-worker test-e2e serve serve-http deploy-cloudflare clean

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

test-worker:
	@echo "==> Running Cloudflare Worker unit tests..."
	node --test worker/

test-e2e: build-client-e2e
	@echo "==> Running Playwright multi-peer browser E2E tests..."
	cd e2e && npm test

test: test-rust test-worker test-e2e

serve: build-client
	@echo "==> Starting dchat server in HTTPS mode (default port 8443)..."
	cargo run -p server --release -- --port 8443 --static-dir crates/client/dist

serve-http: build-client
	@echo "==> Starting dchat server in HTTP mode (port 3000)..."
	cargo run -p server --release -- --http --port 3000 --static-dir crates/client/dist

deploy-cloudflare: build-client
	@echo "==> Deploying to Cloudflare Workers (needs: npx wrangler login, or CLOUDFLARE_API_TOKEN)..."
	npx wrangler@4 deploy

clean:
	cargo clean
	rm -rf crates/client/dist crates/client/dist-e2e
