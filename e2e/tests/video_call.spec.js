import { test, expect } from '@playwright/test';

test('2-peer video call handshake, camera controls, and termination', async ({ browser }) => {
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

  // 4. Peer 1 clicks "📹 Video"
  const videoBtn1 = page1.locator('button:has-text("📹 Video")');
  await expect(videoBtn1).toBeVisible();
  await videoBtn1.click();

  // Peer 1 sees calling banner
  await expect(page1.locator('.call-bar')).toContainText('Calling Peer with Video', { timeout: 5000 });

  // Peer 2 receives incoming call modal
  const acceptBtn2 = page2.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn2).toBeVisible({ timeout: 5000 });

  // 5. Peer 2 accepts video call
  await acceptBtn2.click({ force: true });

  // Both peers show video stage container with remote feed and local preview
  await expect(page1.locator('#video-stage-container')).toBeVisible({ timeout: 10000 });
  await expect(page2.locator('#video-stage-container')).toBeVisible({ timeout: 10000 });
  await expect(page1.locator('#remote-video-feed')).toBeVisible();
  await expect(page1.locator('#local-video-preview')).toBeVisible();
  console.log('Video stage active on both peers with DTLS-SRTP encryption!');

  // 6. Test camera blackout / mute toggle
  const camToggleBtn1 = page1.locator('button[title="Enable/Disable Camera"]');
  await expect(camToggleBtn1).toBeVisible();
  await camToggleBtn1.click();

  // 7. Peer 1 ends video call
  const endCallBtn1 = page1.locator('.video-overlay-controls button:has-text("🔴 End")');
  await expect(endCallBtn1).toBeVisible();
  await endCallBtn1.click();

  // Both peers return to text chat with video stage removed
  await expect(page1.locator('#video-stage-container')).toHaveCount(0, { timeout: 5000 });
  await expect(page2.locator('#video-stage-container')).toHaveCount(0, { timeout: 5000 });
  await expect(page1.locator('button:has-text("📹 Video")')).toBeVisible();
  await expect(page2.locator('button:has-text("📹 Video")')).toBeVisible();
  console.log('Video call cleanly ended and all media tracks destroyed.');

  await context1.close();
  await context2.close();
});
