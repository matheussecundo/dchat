import { test, expect } from '@playwright/test';
import { expectNoStorage, expectVideoFrames, joinVoice, twoMembers, videoTile, voiceChip } from './helpers.js';

const videoSenderCount = (page) => page.evaluate(() => window.__pcs
  .filter((pc) => pc.connectionState === 'connected')
  .map((pc) => pc.getSenders().filter((s) => s.track?.kind === 'video' || s.track === null).length));

test('screen share in the lounge: share, switch to camera on the same sender, stop', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  await joinVoice(ana.page);
  await joinVoice(bo.page);

  await ana.page.locator('#screen-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 10000 });
  await expectVideoFrames(bo.page, 'Ana');
  await expect(ana.page.locator('#screen-btn')).toHaveClass(/active/);

  // One video source at a time: the camera replaces the screen on the existing sender.
  const sendersBefore = await videoSenderCount(ana.page);
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectVideoFrames(bo.page, 'Ana');
  expect(await videoSenderCount(ana.page)).toEqual(sendersBefore);

  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'none', { timeout: 10000 });
  await expect(videoTile(bo.page, 'Ana')).toHaveCount(0);

  await expectNoStorage(ana.page);
  await expectNoStorage(bo.page);
  await ana.context.close();
  await bo.context.close();
});
