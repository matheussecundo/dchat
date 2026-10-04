.PHONY: all build build-client build-client-e2e build-server build-relay build-host-agent relay-image test test-rust test-worker test-e2e serve serve-http deploy-cloudflare clean

all: build test

build: build-client build-server

build-client:
	@echo "==> Building WebAssembly client with Trunk..."
	cd crates/client && trunk build index.html --release

build-host-agent:
	@echo "==> Building dchat-host (remote-control companion app)..."
	cargo build -p host-agent --release

build-client-e2e:
	@echo "==> Building WebAssembly client with E2E test hooks (dist-e2e, never deployed)..."
	cd crates/client && DCHAT_HOST_DOWNLOAD_URL=https://downloads.example.test/dchat-host trunk build index.html --release --features e2e-hooks --dist dist-e2e

build-server:
	@echo "==> Building Axum server..."
	cargo build -p server --release

test-rust:
	@echo "==> Running Rust unit and integration tests..."
	cargo test --workspace

build-relay:
	@echo "==> Building dchat-relay (release)..."
	cargo build -p relay --release --locked

relay-image:
	@echo "==> Building the dchat-relay container image..."
	docker build -f crates/relay/Dockerfile -t dchat-relay .

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
