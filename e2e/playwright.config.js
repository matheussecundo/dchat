import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  timeout: 30000,
  expect: {
    timeout: 10000,
  },
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL: 'http://127.0.0.1:3333',
    headless: true,
    ignoreHTTPSErrors: true,
    launchOptions: {
      args: [
        '--use-fake-ui-for-media-stream',
        '--use-fake-device-for-media-stream',
      ],
    },
  },
  webServer: {
    command: 'cd .. && cargo run -p server -- --http --port 3333 --static-dir crates/client/dist',
    url: 'http://127.0.0.1:3333/health',
    reuseExistingServer: true,
    timeout: 60000,
  },
});
