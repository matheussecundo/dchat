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
        '--autoplay-policy=user-gesture-required',
        '--auto-select-desktop-capture-source=Entire screen',
      ],
    },
  },
  webServer: [
    {
      command: 'cd .. && cargo run -p server -- --http --port 3333 --static-dir crates/client/dist-e2e',
      url: 'http://127.0.0.1:3333/health',
      reuseExistingServer: true,
      timeout: 60000,
    },
    {
      // The remote-control companion app, recording instead of injecting.
      command: 'cd .. && cargo run -p host-agent -- --mock-injector --test-code TEST-0000 --port 7499 --allow-origin http://127.0.0.1:3333 --no-hotkey',
      url: 'http://127.0.0.1:7499/health',
      reuseExistingServer: true,
      timeout: 180000,
    },
  ],
});
