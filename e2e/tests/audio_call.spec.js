import { test, expect } from '@playwright/test';
import { expectAudioFrom, inboundAudioBytes, joinVoice, sentTracks, twoMembers, voiceChip } from './helpers.js';

test('voice lounge with 2 members: audio both ways, mic mute, speaker mute, leave and rejoin', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);

  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expectAudioFrom(ana.page, 1);
  await expectAudioFrom(bo.page, 1);

  // One hidden <audio> per member plays their stream (DTLS-SRTP P2P).
  const remoteAudio = ana.page.locator('audio.remote-audio');
  await expect(remoteAudio).toHaveCount(1);
  await expect.poll(() => remoteAudio.evaluate((a) => !!a.srcObject && !a.paused)).toBe(true);

  // Mic mute disables the sent track; the room sees it.
  await ana.page.locator('#mic-btn').click();
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/);
  expect((await sentTracks(ana.page, 'audio'))[0].enabled).toBe(false);
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-mic', 'off');
  await ana.page.locator('#mic-btn').click();
  expect((await sentTracks(ana.page, 'audio'))[0].enabled).toBe(true);

  // Speaker mute silences incoming audio locally only.
  await bo.page.locator('#speaker-btn').click();
  await expect(bo.page.locator('#speaker-btn')).toHaveClass(/muted/);
  expect(await bo.page.locator('audio.remote-audio').evaluateAll((els) => els.every((a) => a.muted))).toBe(true);
  await expect(voiceChip(ana.page, 'Bo')).toHaveAttribute('data-mic', 'on');
  await bo.page.locator('#speaker-btn').click();
  expect(await bo.page.locator('audio.remote-audio').evaluateAll((els) => els.every((a) => !a.muted))).toBe(true);

  // Leaving stops sending (sender kept, track replaced with none) and releases the mic.
  await ana.page.locator('#leave-voice-btn').click();
  await expect(ana.page.locator('#join-voice-btn')).toBeVisible();
  await expect.poll(() => sentTracks(ana.page, 'audio')).toEqual([null]);
  await expect(bo.page.locator('.voice-chip')).toHaveCount(1);

  // Rejoining resumes audio on the same link.
  await joinVoice(ana.page);
  const before = await inboundAudioBytes(bo.page);
  await expect.poll(() => inboundAudioBytes(bo.page), { timeout: 10000 }).toBeGreaterThan(before + 1000);

  await ana.context.close();
  await bo.context.close();
});
