import { test, expect } from '@playwright/test';

test('2-peer ephemeral WebRTC P2P chat, zero storage, and memory wipe', async ({ browser }) => {
  // 1. Create two isolated browser contexts representing Peer 1 and Peer 2
  const context1 = await browser.newContext();
  const context2 = await browser.newContext();

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  // Log console errors if any
  page1.on('console', msg => {
    if (msg.type() === 'error') console.log('Page1 ERROR:', msg.text());
  });
  page2.on('console', msg => {
    if (msg.type() === 'error') console.log('Page2 ERROR:', msg.text());
  });

  // 2. Peer 1 opens the app
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');

  // Peer 1 URL should have generated room and key in hash
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();
  console.log('Peer 1 session URL:', peer1Url);

  // Peer 1 status should be waiting for peer
  await expect(page1.locator('.status-indicator')).toContainText('Waiting for Peer');

  // 3. Peer 2 opens the exact URL
  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  // 4. Wait for WebRTC P2P connection to be established on both peers
  console.log('Waiting for WebRTC P2P connection...');
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  console.log('P2P connection established on both peers!');

  // 5. Peer 1 sends a message to Peer 2
  const message1 = 'Hello from Peer 1 - completely ephemeral!';
  const input1 = page1.locator('footer.input-bar input');
  await input1.fill(message1);
  await page1.locator('footer.input-bar button:has-text("Send")').click();

  // Verify message appears on Peer 1 and Peer 2
  await expect(page1.locator('.chat-container')).toContainText(message1);
  await expect(page2.locator('.chat-container')).toContainText(message1, { timeout: 5000 });
  console.log('Peer 2 received message from Peer 1!');

  // 6. Peer 2 replies to Peer 1
  const message2 = 'Hello back from Peer 2 - verified zero-knowledge!';
  const input2 = page2.locator('footer.input-bar input');
  await input2.fill(message2);
  await page2.locator('footer.input-bar button:has-text("Send")').click();

  // Verify reply appears on both peers
  await expect(page2.locator('.chat-container')).toContainText(message2);
  await expect(page1.locator('.chat-container')).toContainText(message2, { timeout: 5000 });
  console.log('Peer 1 received reply from Peer 2!');

  // 7. Verify Zero Persistence Invariant: no localStorage, no sessionStorage
  const p1Storage = await page1.evaluate(() => ({
    local: localStorage.length,
    session: sessionStorage.length,
  }));
  const p2Storage = await page2.evaluate(() => ({
    local: localStorage.length,
    session: sessionStorage.length,
  }));

  expect(p1Storage.local).toBe(0);
  expect(p1Storage.session).toBe(0);
  expect(p2Storage.local).toBe(0);
  expect(p2Storage.session).toBe(0);
  console.log('Zero persistence confirmed: localStorage & sessionStorage are empty.');

  // 8. Verify Ephemeral Memory Destruction: Reloading Peer 1 wipes all messages
  await page1.reload();
  await page1.waitForSelector('text=🔒 dchat');
  await expect(page1.locator('.chat-container')).toContainText('Ephemeral P2P Encrypted Session');
  await expect(page1.locator('.message-bubble')).toHaveCount(0);
  console.log('Memory wipe confirmed: chat history destroyed on reload.');

  await context1.close();
  await context2.close();
});

test('custom URL fragment params survive room/key initialization', async ({ page }) => {
  const relay = 'ws://127.0.0.1:3333/nostr';
  await page.goto(`/#relays=${relay}&future=1`);
  await page.waitForSelector('text=🔒 dchat');

  await page.waitForFunction(() => window.location.hash.includes('room=') && window.location.hash.includes('&key='));
  const hash = await page.evaluate(() => window.location.hash);
  expect(hash).toContain(`relays=${relay}`);
  expect(hash).toContain('future=1');

  // The custom relay is the one used for signaling.
  await expect(page.locator('.status-indicator')).toContainText('Waiting for Peer', { timeout: 10000 });
});
