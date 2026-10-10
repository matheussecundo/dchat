import { test, expect } from '@playwright/test';
import { expectDirectMesh, expectNoStorage, joinVoice, twoMembers, voiceChip } from './helpers.js';

test.describe.configure({ timeout: 120000 });

// Chained voice messages start without a tap on each: browsers allow that once the page has
// been used (sticky activation), which is the default policy, not the suite's stricter one.
test.use({
  launchOptions: {
    args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required'],
  },
});

const record = (page) => page.locator('footer.input-bar .record-btn');

/** Tap 🎤, record about `ms`, tap ⏹: the take is under review. */
async function recordTake(page, ms = 1500) {
  await record(page).click();
  await expect(page.locator('.recorder-panel .rec-dot')).toBeVisible();
  await expect(record(page)).toHaveText('⏹', { timeout: 10000 });
  await page.waitForTimeout(ms);
  await record(page).click();
  await expect(page.locator('.recorder-review')).toBeVisible({ timeout: 10000 });
}

/** Press and hold 🎤 at its center; returns the center. */
async function hold(page) {
  const box = await record(page).boundingBox();
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await expect(page.locator('.recorder-panel .rec-hint')).toBeVisible({ timeout: 10000 });
  return { x, y };
}

const voiceCards = (page) => page.locator('.media-card[data-media-kind="voice"]');
const currentTime = (locator) => locator.evaluate((el) => el.currentTime);

test('record, listen back, send: the voice message plays for the other member', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  // 🎤 stands in for Send while there is nothing to send.
  await expect(record(ana.page)).toBeVisible();
  await expect(ana.page.locator('footer.input-bar .send-btn')).toBeHidden();
  await ana.page.locator('footer.input-bar input').fill('typing');
  await expect(record(ana.page)).toBeHidden();
  await ana.page.locator('footer.input-bar input').fill('');

  await recordTake(ana.page, 1800);
  const review = ana.page.locator('.recorder-review');
  await expect(review.locator('.voice-bar')).toHaveCount(64);
  await expect(review.locator('.voice-time')).toHaveText(/^0:0[1-3]$/);
  await review.locator('.rec-play').click();
  await expect.poll(() => currentTime(review.locator('.rec-audio'))).toBeGreaterThan(0.2);
  // Nothing is in the chat before Send.
  await expect(voiceCards(bo.page)).toHaveCount(0);
  await review.locator('.rec-send').click();
  await expect(ana.page.locator('.recorder-review')).toHaveCount(0);

  const mine = voiceCards(ana.page);
  await expect(mine).toHaveCount(1);
  // MP3, whatever the browser: the format every browser and phone plays.
  await expect(mine.locator('.media-name')).toHaveText(/^voice-\d{8}-\d{6}\.mp3$/);
  await expect(mine.locator('.voice-save-btn')).toBeVisible();

  const theirs = voiceCards(bo.page);
  await expect(theirs).toHaveCount(1, { timeout: 15000 });
  await expect(theirs.locator('.voice-bar')).toHaveCount(64);
  await expect(theirs.locator('.voice-time')).toHaveText(/^0:0[1-3]$/);
  await expect(theirs.locator('.voice-play')).toBeEnabled({ timeout: 15000 });
  await theirs.locator('.voice-play').click();
  await expect(theirs.locator('.voice-bubble')).toHaveClass(/playing/);
  await expect.poll(() => currentTime(theirs.locator('audio.voice-audio'))).toBeGreaterThan(0.3);
  await expect.poll(() => theirs.locator('.voice-bar.played').count()).toBeGreaterThan(0);
  // What Bo received is an MPEG audio stream (frame sync or an ID3 tag first).
  const head = await theirs.locator('audio.voice-audio').evaluate(async (el) => Array.from(new Uint8Array(await (await fetch(el.src)).arrayBuffer()).slice(0, 3)));
  expect(head[0] === 0xff ? (head[1] & 0xe0) === 0xe0 : String.fromCharCode(...head) === 'ID3').toBe(true);

  for (const m of [ana, bo]) await expectNoStorage(m.page);
  for (const m of [ana, bo]) await m.context.close();
});

test('hold to record: release reviews, slide left cancels, slide up locks; Discard sends nothing', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);

  // Hold, release: the review.
  await hold(ana.page);
  await ana.page.waitForTimeout(800);
  await ana.page.mouse.up();
  await expect(ana.page.locator('.recorder-review')).toBeVisible({ timeout: 10000 });
  await ana.page.locator('.rec-discard').click();
  await expect(ana.page.locator('.recorder-review')).toHaveCount(0);

  // Hold, slide toward the start: cancelled, nothing to review.
  let at = await hold(ana.page);
  await ana.page.waitForTimeout(500);
  await ana.page.mouse.move(at.x - 150, at.y, { steps: 6 });
  await expect(ana.page.locator('.recorder-panel')).toHaveCount(0);
  await ana.page.mouse.up();
  await ana.page.waitForTimeout(300);
  await expect(ana.page.locator('.recorder-review')).toHaveCount(0);
  await expect(record(ana.page)).toHaveText('🎤');

  // Hold, slide up: locked, it keeps recording after the release until ⏹.
  at = await hold(ana.page);
  await ana.page.mouse.move(at.x, at.y - 100, { steps: 6 });
  await expect(record(ana.page)).toHaveText('⏹');
  await ana.page.mouse.up();
  await ana.page.waitForTimeout(600);
  await expect(ana.page.locator('.recorder-panel')).toBeVisible();
  await record(ana.page).click();
  await expect(ana.page.locator('.recorder-review')).toBeVisible({ timeout: 10000 });
  await ana.page.locator('.rec-discard').click();

  await bo.page.waitForTimeout(1000);
  await expect(voiceCards(bo.page)).toHaveCount(0);
  await expect(voiceCards(ana.page)).toHaveCount(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('recording stops by itself at the maximum length and goes to the review', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await ana.page.evaluate(() => window.__dchatMedia.recordCapMs(1500));
  await record(ana.page).click();
  await expect(ana.page.locator('.recorder-review')).toBeVisible({ timeout: 10000 });
  await expect(ana.page.locator('.recorder-review .voice-time')).toHaveText(/^0:0[12]$/);
  for (const m of [ana, bo]) await m.context.close();
});

test('recording mutes the lounge mic and puts it back afterwards', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-mic', 'on', { timeout: 15000 });

  await record(ana.page).click();
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/, { timeout: 10000 });
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-mic', 'off', { timeout: 10000 });
  await ana.page.waitForTimeout(800);
  await record(ana.page).click();
  await expect(ana.page.locator('.recorder-review')).toBeVisible({ timeout: 10000 });
  await expect(ana.page.locator('#mic-btn')).not.toHaveClass(/muted/);
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-mic', 'on', { timeout: 10000 });

  // Muted before recording: stays muted after.
  await ana.page.locator('.rec-discard').click();
  await ana.page.locator('#mic-btn').click();
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/);
  await recordTake(ana.page, 500);
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/);

  for (const m of [ana, bo]) await m.context.close();
});

test('consecutive voice messages play one after the other, one player at a time', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  for (const ms of [900, 900]) {
    await recordTake(ana.page, ms);
    await ana.page.locator('.rec-send').click();
  }
  const cards = voiceCards(bo.page);
  await expect(cards).toHaveCount(2, { timeout: 15000 });
  for (let i = 0; i < 2; i++) await expect(cards.nth(i).locator('.voice-play')).toBeEnabled({ timeout: 15000 });

  await cards.nth(0).locator('.voice-play').click();
  await expect(cards.nth(0).locator('.voice-bubble')).toHaveClass(/playing/);
  // The first ends, the second starts by itself.
  await expect(cards.nth(1).locator('.voice-bubble')).toHaveClass(/playing/, { timeout: 10000 });
  await expect(cards.nth(0).locator('.voice-bubble')).not.toHaveClass(/playing/);

  // Starting one pauses the other.
  await cards.nth(0).locator('.voice-play').click();
  await expect(cards.nth(0).locator('.voice-bubble')).toHaveClass(/playing/);
  await expect(cards.nth(1).locator('.voice-bubble')).not.toHaveClass(/playing/);

  for (const m of [ana, bo]) await m.context.close();
});

test('a take under review survives a rotated link and is sent in the new room', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await recordTake(ana.page, 1000);

  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  await expect(bo.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  await expect(ana.page.locator('.recorder-review')).toBeVisible();
  await ana.page.locator('.rec-send').click();
  const theirs = voiceCards(bo.page);
  await expect(theirs.locator('.voice-play')).toBeEnabled({ timeout: 15000 });
  await theirs.locator('.voice-play').click();
  await expect.poll(() => currentTime(theirs.locator('audio.voice-audio'))).toBeGreaterThan(0.2);

  for (const m of [ana, bo]) await m.context.close();
});
