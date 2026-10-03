import { test, expect } from '@playwright/test';

// The 1:1 call/file flow is replaced by the group model; rewritten in milestone 8b.
test.skip(true, 'Rewritten for group rooms in milestone 8b');

// Reads the mic track currently being sent to the peer.
const senderAudio = (page) => page.evaluate(() => {
  const track = window.__pcs[0].getSenders().find(s => s.track?.kind === 'audio')?.track;
  if (!track) return null;
  const { noiseSuppression, echoCancellation, autoGainControl } = track.getSettings();
  return { id: track.id, enabled: track.enabled, readyState: track.readyState, noiseSuppression, echoCancellation, autoGainControl };
});

const waitForSenderAudio = (page, expected) => page.waitForFunction((exp) => {
  const track = window.__pcs[0].getSenders().find(s => s.track?.kind === 'audio')?.track;
  if (!track || track.readyState !== 'live') return false;
  const settings = track.getSettings();
  return Object.entries(exp).every(([k, v]) => settings[k] === v);
}, expected, { timeout: 10000 });

const startAudioCall = async (caller, callee) => {
  await caller.locator('button:has-text("📞 Audio")').click();
  const acceptBtn = callee.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn).toBeVisible({ timeout: 5000 });
  await acceptBtn.click({ force: true });
  await expect(caller.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  await expect(callee.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
};

test('audio processing checkboxes default on, apply live mid-call, and reset on reload', async ({ browser }) => {
  test.setTimeout(60000);
  const context1 = await browser.newContext({ permissions: ['microphone'] });
  const context2 = await browser.newContext({ permissions: ['microphone'] });

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
  page1.on('pageerror', err => console.log('[P1 ERR]', err));

  await page1.goto('/');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));

  // 1. Before any call: all three switches on and enabled
  await page1.locator('#audio-settings-btn').click();
  for (const id of ['#audio-ns', '#audio-ec', '#audio-agc']) {
    await expect(page1.locator(id)).toBeChecked();
    await expect(page1.locator(id)).toBeEnabled();
  }
  await page1.locator('.modal-content button:has-text("Close")').click();
  await expect(page1.locator('#audio-ns')).toHaveCount(0);

  await page2.goto(page1.url());
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 2. Call starts with the processing requested explicitly
  await startAudioCall(page1, page2);
  await waitForSenderAudio(page1, { noiseSuppression: true, echoCancellation: true, autoGainControl: true });
  const firstTrack = await senderAudio(page1);

  // 3. Mute mic, then turn noise cancellation off mid-call: track is swapped, mute is kept
  await page1.locator('button:has-text("Mute Mic")').click();
  await expect(page1.locator('button:has-text("Unmute Mic")')).toBeVisible();
  await page1.locator('.call-bar button[title="Audio settings"]').click();
  await page1.locator('#audio-ns').uncheck();
  await waitForSenderAudio(page1, { noiseSuppression: false });
  const swapped = await senderAudio(page1);
  expect(swapped.id).not.toBe(firstTrack.id);
  expect(swapped.enabled).toBe(false);
  expect(swapped.echoCancellation).toBe(true);
  await expect(page1.locator('button:has-text("Unmute Mic")')).toBeVisible();

  // 4. Rapid changes collapse to the latest settings
  await page1.locator('#audio-ec').uncheck();
  await page1.locator('#audio-agc').uncheck();
  await waitForSenderAudio(page1, { noiseSuppression: false, echoCancellation: false, autoGainControl: false });
  await expect(page1.locator('#audio-ns')).not.toBeChecked();
  await expect(page1.locator('#audio-ec')).not.toBeChecked();
  await expect(page1.locator('#audio-agc')).not.toBeChecked();
  await page1.locator('.modal-content button:has-text("Close")').click();

  // 5. Swapped track actually flows to the peer once unmuted
  await page1.locator('button:has-text("Unmute Mic")').click();
  const outboundBytes = () => page1.evaluate(async () => {
    let bytes = 0;
    (await window.__pcs[0].getStats()).forEach(r => {
      if (r.type === 'outbound-rtp' && r.kind === 'audio') bytes = r.bytesSent;
    });
    return bytes;
  });
  const bytesBefore = await outboundBytes();
  await page1.waitForTimeout(1000);
  expect(await outboundBytes()).toBeGreaterThan(bytesBefore);

  // 6. Settings carry over to the next call in the same tab
  await page1.locator('.call-bar button:has-text("End Call")').click();
  await expect(page1.locator('button:has-text("📞 Audio")')).toBeVisible({ timeout: 5000 });
  await startAudioCall(page1, page2);
  await waitForSenderAudio(page1, { noiseSuppression: false, echoCancellation: false, autoGainControl: false });

  // 7. Zero persistence: nothing stored, reload restores defaults
  const storage = await page1.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length }));
  expect(storage).toEqual({ local: 0, session: 0 });
  await page1.reload();
  await page1.locator('#audio-settings-btn').click();
  for (const id of ['#audio-ns', '#audio-ec', '#audio-agc']) {
    await expect(page1.locator(id)).toBeChecked();
  }

  await context1.close();
  await context2.close();
});
