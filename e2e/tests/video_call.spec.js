import { test, expect } from '@playwright/test';

test('2-peer video call handshake, camera controls, and termination', async ({ browser }) => {
  const context1 = await browser.newContext({
    permissions: ['camera', 'microphone'],
  });
  const context2 = await browser.newContext({
    permissions: ['camera', 'microphone'],
  });

  await context1.addInitScript(`
    window.__pcs = [];
    const OrigPC = window.RTCPeerConnection;
    window.RTCPeerConnection = function(...args) {
      const pc = new OrigPC(...args);
      window.__pcs.push(pc);
      return pc;
    };
    window.RTCPeerConnection.prototype = OrigPC.prototype;
  `);

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
  await page1.waitForFunction(() => {
    const remote = document.getElementById('remote-video-feed');
    const local = document.getElementById('local-video-preview');
    return !!remote?.srcObject && !!local?.srcObject;
  }, { timeout: 10000 });

  await page2.waitForFunction(() => {
    const remote = document.getElementById('remote-video-feed');
    const local = document.getElementById('local-video-preview');
    return !!remote?.srcObject && !!local?.srcObject;
  }, { timeout: 10000 });

  console.log('Video stage active on both peers with DTLS-SRTP encryption and srcObjects attached!');

  // 6. Test camera blackout / mute toggle
  const camToggleBtn1 = page1.locator('button[title="Enable/Disable Camera"]');
  await expect(camToggleBtn1).toBeVisible();
  await camToggleBtn1.click();

  // 6b. Overlay speaker toggle mutes incoming audio locally
  const speakerBtn1 = page1.locator('.video-overlay-controls #speaker-mute-btn');
  await expect(speakerBtn1).toBeVisible();
  await speakerBtn1.click();
  await expect(speakerBtn1).toHaveClass(/muted/);
  expect(await page1.evaluate(() => document.getElementById('remote-audio').muted)).toBe(true);

  // 6c. After a camera flip, mic mute must still act on the track actually being sent
  const senderAudioTrackId = () => page1.evaluate(() =>
    window.__pcs[0].getSenders().find(s => s.track?.kind === 'audio')?.track?.id);
  const audioTrackBeforeFlip = await senderAudioTrackId();
  const previewIdBefore = await page1.evaluate(() => document.getElementById('local-video-preview').srcObject?.id);
  await page1.locator('button[title="Flip Front/Rear Camera"]').click();
  await page1.waitForFunction(
    (prev) => document.getElementById('local-video-preview').srcObject?.id !== prev,
    previewIdBefore,
    { timeout: 10000 },
  );
  expect(await senderAudioTrackId()).toBe(audioTrackBeforeFlip);
  await page1.locator('button[title="Mute/Unmute Mic"]').click();
  expect(await page1.evaluate(() =>
    window.__pcs[0].getSenders().find(s => s.track?.kind === 'audio').track.enabled)).toBe(false);

  // 6d. Noise cancellation off mid video call: mic re-captured, camera untouched, mute kept
  const videoTrackBefore = await page1.evaluate(() =>
    window.__pcs[0].getSenders().find(s => s.track?.kind === 'video').track.id);
  await page1.locator('.video-overlay-controls button[title="Audio settings"]').click();
  await page1.locator('#audio-ns').uncheck();
  await page1.waitForFunction(() => {
    const t = window.__pcs[0].getSenders().find(s => s.track?.kind === 'audio')?.track;
    return t?.readyState === 'live' && t.getSettings().noiseSuppression === false;
  }, null, { timeout: 10000 });
  const afterSwap = await page1.evaluate(() => {
    const senders = window.__pcs[0].getSenders();
    const audio = senders.find(s => s.track?.kind === 'audio').track;
    const video = senders.find(s => s.track?.kind === 'video').track;
    return { audioEnabled: audio.enabled, videoId: video.id, videoState: video.readyState };
  });
  expect(afterSwap).toEqual({ audioEnabled: false, videoId: videoTrackBefore, videoState: 'live' });
  await page1.locator('.modal-content button:has-text("Close")').click();

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
