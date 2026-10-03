import { test, expect } from '@playwright/test';
import { expectVideoFrames, joinVoice, sentTracks, twoMembers, videoTile, voiceChip } from './helpers.js';

test('lounge video: camera tiles, camera flip keeps the mic, camera off, leave hides the grid', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect(ana.page.locator('#video-grid')).toHaveCount(0);

  // Ana turns her camera on: both see her tile render frames.
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectVideoFrames(bo.page, 'Ana');
  await expectVideoFrames(ana.page, 'Ana');

  // Flipping the camera swaps only the video track; the mic keeps flowing.
  const audioBefore = (await sentTracks(ana.page, 'audio'))[0];
  const videoBefore = (await sentTracks(ana.page, 'video'))[0];
  await ana.page.locator('#flip-camera-btn').click();
  await expect.poll(async () => (await sentTracks(ana.page, 'video'))[0]?.id, { timeout: 10000 }).not.toBe(videoBefore.id);
  const audioAfter = (await sentTracks(ana.page, 'audio'))[0];
  expect(audioAfter.id).toBe(audioBefore.id);
  expect(audioAfter.readyState).toBe('live');

  // Bo's camera too: two tiles on both sides, fullscreen available.
  await bo.page.locator('#camera-btn').click();
  await expectVideoFrames(ana.page, 'Bo');
  await expect(bo.page.locator('#video-grid .tile')).toHaveCount(2);
  await expect(ana.page.locator('.grid-fullscreen')).toBeVisible();

  // Camera off removes the tile but keeps the call.
  await ana.page.locator('#camera-btn').click();
  await expect(videoTile(bo.page, 'Ana')).toHaveCount(0, { timeout: 10000 });
  await expect.poll(() => sentTracks(ana.page, 'video')).toEqual([null]);
  await expect(voiceChip(bo.page, 'Ana')).toBeVisible();

  // Leaving voice hides the grid for Bo's own view of Ana and ends Bo's video for Ana.
  await bo.page.locator('#leave-voice-btn').click();
  await expect(bo.page.locator('#video-grid')).toHaveCount(0);
  await expect(ana.page.locator('#video-grid')).toHaveCount(0, { timeout: 10000 });

  await ana.context.close();
  await bo.context.close();
});
