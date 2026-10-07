import { test, expect } from '@playwright/test';
import { inboundAudioBytes, joinVoice, twoMembers, waitForSenderAudio } from './helpers.js';

// The mic track Ana currently sends on her only link.
const senderAudio = (page) => page.evaluate(() => {
  const pc = window.__pcs.find((p) => p.connectionState === 'connected');
  const track = pc?.getSenders().find((s) => s.track?.kind === 'audio')?.track;
  if (!track) return null;
  const { noiseSuppression, echoCancellation, autoGainControl } = track.getSettings();
  return { id: track.id, enabled: track.enabled, readyState: track.readyState, noiseSuppression, echoCancellation, autoGainControl };
});

test('audio processing checkboxes default on, apply live in voice, carry over, and reset on reload', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);

  // 1. Before joining voice: all three switches on and enabled.
  await ana.page.locator('#audio-settings-btn').click();
  for (const id of ['#audio-ns', '#audio-ec', '#audio-agc']) {
    await expect(ana.page.locator(id)).toBeChecked();
    await expect(ana.page.locator(id)).toBeEnabled();
  }
  await ana.page.locator('.modal-content button:has-text("Close")').click();
  await expect(ana.page.locator('#audio-ns')).toHaveCount(0);

  // 2. Voice starts with the processing requested explicitly.
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await waitForSenderAudio(ana.page, { noiseSuppression: true, echoCancellation: true, autoGainControl: true });
  const firstTrack = await senderAudio(ana.page);

  // 3. Mute, then turn noise cancellation off: the track is swapped and stays muted.
  await ana.page.locator('#mic-btn').click();
  await ana.page.locator('#audio-settings-btn').click();
  await ana.page.locator('#audio-ns').uncheck();
  await waitForSenderAudio(ana.page, { noiseSuppression: false });
  const swapped = await senderAudio(ana.page);
  expect(swapped.id).not.toBe(firstTrack.id);
  expect(swapped.enabled).toBe(false);
  expect(swapped.echoCancellation).toBe(true);

  // 4. Rapid changes collapse to the latest settings.
  await ana.page.locator('#audio-ec').uncheck();
  await ana.page.locator('#audio-agc').uncheck();
  await waitForSenderAudio(ana.page, { noiseSuppression: false, echoCancellation: false, autoGainControl: false });
  await ana.page.locator('.modal-content button:has-text("Close")').click();
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/);

  // 5. The swapped track flows to Bo once unmuted.
  await ana.page.locator('#mic-btn').click();
  const before = await inboundAudioBytes(bo.page);
  await expect.poll(() => inboundAudioBytes(bo.page), { timeout: 10000 }).toBeGreaterThan(before + 1000);

  // 6. Settings carry over to the next time Ana joins voice in this tab.
  await ana.page.locator('#leave-voice-btn').click();
  await joinVoice(ana.page);
  await waitForSenderAudio(ana.page, { noiseSuppression: false, echoCancellation: false, autoGainControl: false });

  // 7. Zero persistence: nothing stored, reload restores defaults.
  const storage = await ana.page.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length }));
  expect(storage).toEqual({ local: 0, session: 0 });
  await ana.page.reload();
  await ana.page.locator('#enter-room-btn').click();
  await ana.page.locator('#audio-settings-btn').click();
  for (const id of ['#audio-ns', '#audio-ec', '#audio-agc']) {
    await expect(ana.page.locator(id)).toBeChecked();
  }

  await ana.context.close();
  await bo.context.close();
});
