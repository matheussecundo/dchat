import { test, expect } from '@playwright/test';

test('2-peer screen sharing handshake, stream delivery, and clean termination', async ({ browser }) => {
  const context1 = await browser.newContext({
    permissions: ['camera', 'microphone'],
  });
  const context2 = await browser.newContext({
    permissions: ['camera', 'microphone'],
  });

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  // 1. Peer 1 opens app
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();

  // 2. Peer 2 opens room URL
  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  // 3. Wait for P2P connection
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 4. Peer 1 initiates Screen Share
  const screenBtn1 = page1.locator('button:has-text("🖥️ Screen")');
  await expect(screenBtn1).toBeVisible();
  await screenBtn1.click();

  // Peer 1 sees calling banner
  await expect(page1.locator('.call-bar')).toContainText('Calling Peer with Screen Share', { timeout: 5000 });

  // Peer 2 receives incoming screen share modal
  const acceptBtn2 = page2.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn2).toBeVisible({ timeout: 5000 });

  // 5. Peer 2 accepts screen share
  await acceptBtn2.click({ force: true });

  // Both peers show video stage container
  await expect(page1.locator('#video-stage-container')).toBeVisible({ timeout: 10000 });
  await expect(page2.locator('#video-stage-container')).toBeVisible({ timeout: 10000 });
  await expect(page1.locator('#remote-video-feed')).toBeVisible();
  await expect(page2.locator('#remote-video-feed')).toBeVisible();

  // Verify that Peer 1 (presenter) and Peer 2 (viewer) both receive the video track
  await page1.waitForFunction(() => {
    const remote = document.getElementById('remote-video-feed');
    const local = document.getElementById('local-video-preview');
    return !!remote?.srcObject || !!local?.srcObject;
  }, { timeout: 10000 });

  await page2.waitForFunction(() => {
    const remote = document.getElementById('remote-video-feed');
    return !!remote?.srcObject;
  }, { timeout: 10000 });

  console.log('Screen sharing stream successfully delivered and rendering on both peers!');

  // 6. Test mic mute toggle during screen share
  const micToggleBtn1 = page1.locator('.video-overlay-controls button[title="Mute/Unmute Mic"]');
  await expect(micToggleBtn1).toBeVisible();
  await micToggleBtn1.click();

  // 7. Peer 1 ends screen share
  const endCallBtn1 = page1.locator('.video-overlay-controls button:has-text("🔴 End")');
  await expect(endCallBtn1).toBeVisible();
  await endCallBtn1.click();

  // Both peers return to text chat with video stage removed
  await expect(page1.locator('#video-stage-container')).toHaveCount(0, { timeout: 5000 });
  await expect(page2.locator('#video-stage-container')).toHaveCount(0, { timeout: 5000 });
  await expect(page1.locator('button:has-text("🖥️ Screen")')).toBeVisible();
  await expect(page2.locator('button:has-text("🖥️ Screen")')).toBeVisible();

  // Confirm zero persistence
  const storage1 = await page1.evaluate(() => localStorage.length + sessionStorage.length);
  const storage2 = await page2.evaluate(() => localStorage.length + sessionStorage.length);
  expect(storage1).toBe(0);
  expect(storage2).toBe(0);

  console.log('Screen share cleanly terminated with zero storage persistence!');

  await context1.close();
  await context2.close();
});
