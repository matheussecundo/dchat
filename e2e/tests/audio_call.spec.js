import { test, expect } from '@playwright/test';

test('2-peer audio call handshake, mute toggle, and end call', async ({ browser }) => {
  const context1 = await browser.newContext({
    permissions: ['microphone'],
  });
  const context2 = await browser.newContext({
    permissions: ['microphone'],
  });

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  // 1. Peer 1 opens app
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();

  // 2. Peer 2 opens the exact room URL
  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  // 3. Wait for P2P connection
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 4. Peer 1 clicks "📞 Audio Call"
  const callBtn1 = page1.locator('button:has-text("📞 Audio Call")');
  await expect(callBtn1).toBeVisible();
  await callBtn1.click();

  // Peer 1 should see "Calling Peer..."
  await expect(page1.locator('.call-bar')).toContainText('Calling Peer...', { timeout: 5000 });

  // Peer 2 should see incoming call prompt with Accept button
  const acceptBtn2 = page2.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn2).toBeVisible({ timeout: 5000 });

  // 5. Peer 2 accepts the call
  await acceptBtn2.click({ force: true });

  // Both peers should transition to "Audio Call Active"
  await expect(page1.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  await expect(page2.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  console.log('Audio call successfully established between Peer 1 and Peer 2!');

  // 6. Test microphone mute toggle on Peer 1
  const muteBtn1 = page1.locator('button:has-text("Mute Mic")');
  await expect(muteBtn1).toBeVisible();
  await muteBtn1.click();
  await expect(page1.locator('button:has-text("Unmute Mic")')).toBeVisible();

  // Unmute again
  await page1.locator('button:has-text("Unmute Mic")').click();
  await expect(page1.locator('button:has-text("Mute Mic")')).toBeVisible();
  console.log('Microphone mute/unmute verified.');

  // 7. Peer 2 ends the call
  const endBtn2 = page2.locator('.call-bar button:has-text("End Call")');
  await expect(endBtn2).toBeVisible();
  await endBtn2.click();

  // Both peers should return to idle call state
  await expect(page1.locator('button:has-text("📞 Audio Call")')).toBeVisible({ timeout: 5000 });
  await expect(page2.locator('button:has-text("📞 Audio Call")')).toBeVisible({ timeout: 5000 });
  await expect(page1.locator('.call-bar')).toHaveCount(0);
  await expect(page2.locator('.call-bar')).toHaveCount(0);
  console.log('Audio call ended and cleaned up cleanly.');

  await context1.close();
  await context2.close();
});
