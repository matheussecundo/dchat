import { test, expect } from '@playwright/test';
import {
  expectVideoFrames, joinVoice, sentTracks, twoMembers, videoSenderParams, videoTile, voiceChip,
} from './helpers.js';

/** The video grid never scrolls and every tile lies inside it. */
async function expectGridFits(page) {
  await expect.poll(() => page.evaluate(() => {
    const grid = document.getElementById('video-grid');
    const box = grid.getBoundingClientRect();
    const inside = [...grid.querySelectorAll('.tile')].every((t) => {
      const r = t.getBoundingClientRect();
      return r.width > 0 && r.top >= box.top - 1 && r.bottom <= box.bottom + 1 && r.left >= box.left - 1 && r.right <= box.right + 1;
    });
    return inside && grid.scrollHeight <= grid.clientHeight && grid.scrollWidth <= grid.clientWidth;
  })).toBe(true);
}

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

  // Bo's camera too: two tiles on both sides, fitted to the stage without scrolling.
  await bo.page.locator('#camera-btn').click();
  await expectVideoFrames(ana.page, 'Bo');
  await expect(bo.page.locator('#video-grid .tile')).toHaveCount(2);
  await expectGridFits(ana.page);

  // Fullscreen: the whole grid from the voice bar, then a single tile by double-click.
  await ana.page.locator('#fullscreen-btn').click();
  await expect.poll(() => ana.page.evaluate(() => document.fullscreenElement?.id)).toBe('video-grid');
  await expectGridFits(ana.page);
  await expect(ana.page.locator('.grid-fullscreen')).toHaveText('✕');
  await ana.page.locator('.grid-fullscreen').click();
  await expect.poll(() => ana.page.evaluate(() => document.fullscreenElement === null)).toBe(true);
  await videoTile(ana.page, 'Bo').dblclick();
  await expect.poll(() => ana.page.evaluate(() => document.fullscreenElement?.classList.contains('tile'))).toBe(true);
  await ana.page.evaluate(() => document.exitFullscreen());

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

test('camera HD: flipping the camera keeps 1280×720 and the HD encoder settings', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  await ana.page.locator('#audio-settings-btn').click();
  await ana.page.locator('#camera-preset-hd').check();
  await ana.page.locator('.modal-content button:has-text("Close")').click();
  await joinVoice(ana.page);
  await joinVoice(bo.page);

  await ana.page.locator('#camera-btn').click();
  await expectVideoFrames(bo.page, 'Ana');
  const hd = (p) => p && { width: p.width, height: p.height, maxFramerate: p.maxFramerate, degradationPreference: p.degradationPreference };
  const expected = { width: 1280, height: 720, maxFramerate: 30, degradationPreference: 'balanced' };
  await expect.poll(async () => (await videoSenderParams(ana.page)).map(hd), { timeout: 15000 }).toEqual([expected]);

  const before = (await sentTracks(ana.page, 'video'))[0];
  await ana.page.locator('#flip-camera-btn').click();
  await expect.poll(async () => (await sentTracks(ana.page, 'video'))[0]?.id, { timeout: 10000 }).not.toBe(before.id);
  await expect.poll(async () => (await videoSenderParams(ana.page)).map(hd), { timeout: 15000 }).toEqual([expected]);
  await expectVideoFrames(bo.page, 'Ana');

  await ana.context.close();
  await bo.context.close();
});
