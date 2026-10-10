import { test, expect } from '@playwright/test';
import { expectDirectMesh, expectNoStorage, inviteFrom, joinRoom, newMember, sendMessage, twoMembers } from './helpers.js';

test.describe.configure({ timeout: 120000 });

/** A real picture drawn by the page's canvas (`noise`: hard to compress, so larger). */
async function picture(page, { width = 320, height = 240, type = 'image/png', color = '#3b82f6', noise = false } = {}) {
  const b64 = await page.evaluate(({ width, height, type, color, noise }) => {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const g = canvas.getContext('2d');
    g.fillStyle = color;
    g.fillRect(0, 0, width, height);
    g.fillStyle = '#fff';
    g.fillRect(10, 10, width / 2, height / 2);
    if (noise) {
      const img = g.getImageData(0, 0, width, height);
      for (let i = 0; i < img.data.length; i++) if (i % 4 !== 3) img.data[i] = Math.random() * 255;
      g.putImageData(img, 0, 0);
    }
    return canvas.toDataURL(type, 0.92).split(',')[1];
  }, { width, height, type, color, noise });
  return Buffer.from(b64, 'base64');
}

/** A short real video (WebM) recorded from a changing canvas. */
async function clip(page, ms = 2000) {
  const b64 = await page.evaluate(async (ms) => {
    const canvas = document.createElement('canvas');
    canvas.width = 320;
    canvas.height = 240;
    const g = canvas.getContext('2d');
    let hue = 0;
    const paint = setInterval(() => {
      g.fillStyle = `hsl(${(hue += 15) % 360} 80% 50%)`;
      g.fillRect(0, 0, 320, 240);
    }, 40);
    const recorder = new MediaRecorder(canvas.captureStream(25), { mimeType: 'video/webm' });
    const parts = [];
    recorder.ondataavailable = (e) => parts.push(e.data);
    const stopped = new Promise((resolve) => (recorder.onstop = resolve));
    recorder.start();
    await new Promise((resolve) => setTimeout(resolve, ms));
    recorder.stop();
    await stopped;
    clearInterval(paint);
    const bytes = new Uint8Array(await new Blob(parts).arrayBuffer());
    let s = '';
    for (const b of bytes) s += String.fromCharCode(b);
    return btoa(s);
  }, ms);
  return Buffer.from(b64, 'base64');
}

/** Stage `files` with 📎, wait for their previews, and send (with an optional caption). */
async function shareMedia(page, files, caption) {
  await page.setInputFiles('#file-input-hidden', files);
  await expect(page.locator('.attachment-chip')).toHaveCount(files.length);
  await expect(page.locator('.attachment-chip[data-ready="false"]')).toHaveCount(0, { timeout: 15000 });
  if (caption) await page.locator('footer.input-bar input').fill(caption);
  await page.locator('footer.input-bar .send-btn').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
}

const heldMedia = (page) => page.evaluate(() => window.__dchatMedia.heldMedia());
const requestsSent = (page) => page.evaluate(() => window.__dchat.fileRequestsSent());
const imageShown = (img) => img.evaluate((el) => el.complete && el.naturalWidth > 0);

/** Click the card's Download and return the saved bytes (no save picker: a browser download). */
async function saveVia(page, button) {
  await page.evaluate(() => { delete window.showSaveFilePicker; });
  const downloadPromise = page.waitForEvent('download');
  await button.click();
  const download = await downloadPromise;
  const chunks = [];
  for await (const chunk of await download.createReadStream()) chunks.push(chunk);
  return { name: download.suggestedFilename(), bytes: Buffer.concat(chunks) };
}

test('a photo shows in the chat by itself, opens fullscreen, and Download saves the copy already here', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const photo = await picture(ana.page, { width: 640, height: 480 });
  await shareMedia(ana.page, [{ name: 'beach.png', mimeType: 'image/png', buffer: photo }], 'Look at this');

  // The sender sees its own picture at once, straight from its file.
  await expect(ana.page.locator('.media-card .media-image')).toBeVisible();
  expect(await imageShown(ana.page.locator('.media-card .media-image'))).toBe(true);

  // Bo's card loads on its own (small, on screen) and keeps the photo's shape.
  const boCard = bo.page.locator('.media-card[data-media-kind="image"]');
  await expect(boCard).toContainText('beach.png', { timeout: 15000 });
  await expect(boCard).toContainText('Look at this');
  const image = boCard.locator('.media-image');
  await expect(image).toBeVisible({ timeout: 15000 });
  await expect.poll(() => imageShown(image)).toBe(true);
  const frame = await boCard.locator('.media-frame').boundingBox();
  expect(Math.round((frame.width / frame.height) * 100) / 100).toBe(1.33);
  expect(await requestsSent(bo.page)).toBe(1);

  // Fullscreen viewer: opens on a tap, closes with Esc.
  await image.click();
  await expect(bo.page.locator('.media-viewer .media-viewer-image')).toBeVisible();
  await bo.page.keyboard.press('Escape');
  await expect(bo.page.locator('.media-viewer')).toHaveCount(0);
  // A tap on the picture keeps it open; one beside it closes it.
  await image.click();
  await bo.page.locator('.media-viewer-image').click();
  await expect(bo.page.locator('.media-viewer')).toBeVisible();
  await bo.page.locator('.media-viewer-stage').click({ position: { x: 4, y: 4 } });
  await expect(bo.page.locator('.media-viewer')).toHaveCount(0);

  // Download (from the viewer, and from the card) saves the bytes held here: nothing pulled again.
  await image.click();
  const fromViewer = await saveVia(bo.page, bo.page.locator('.media-viewer-download'));
  expect(fromViewer.name).toBe('beach.png');
  expect(fromViewer.bytes.equals(photo)).toBe(true);
  await bo.page.locator('.media-viewer-close').click();
  const fromCard = await saveVia(bo.page, boCard.locator('.media-download-btn'));
  expect(fromCard.bytes.equals(photo)).toBe(true);
  expect(await requestsSent(bo.page)).toBe(1);

  for (const m of [ana, bo]) await expectNoStorage(m.page);
  for (const m of [ana, bo]) await m.context.close();
});

test('history loads pictures only once they scroll into view', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await shareMedia(ana.page, [{ name: 'old.png', mimeType: 'image/png', buffer: await picture(ana.page) }]);
  for (let i = 0; i < 30; i++) await sendMessage(ana.page, `filler ${i}`);
  await expect(bo.page.locator('.message-row', { hasText: 'filler 29' })).toBeVisible({ timeout: 15000 });

  const cy = await newMember(browser, 'Cy');
  await joinRoom(cy.page, inviteFrom(ana.page.url()), 'Cy');
  await expect(cy.page.locator('.message-row', { hasText: 'filler 29' })).toBeVisible({ timeout: 20000 });
  const old = cy.page.locator('.media-card', { hasText: 'old.png' });
  await expect(old).toHaveCount(1);
  // Out of sight at the top of the history: not pulled.
  await cy.page.waitForTimeout(1500);
  expect(await requestsSent(cy.page)).toBe(0);
  await expect(old.locator('.media-image')).toHaveCount(0);

  await old.scrollIntoViewIfNeeded();
  await expect(old.locator('.media-image')).toBeVisible({ timeout: 15000 });
  await expect.poll(() => imageShown(old.locator('.media-image'))).toBe(true);
  expect(await requestsSent(cy.page)).toBe(1);

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('media past 16 MB waits for a tap; video plays inline and a reaction does not restart it', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  // A JPEG with 17 MB of padding after its end: a valid picture, too large to load by itself.
  const big = Buffer.concat([await picture(ana.page, { type: 'image/jpeg' }), Buffer.alloc(17 * 1024 * 1024)]);
  const video = await clip(ana.page);
  await shareMedia(ana.page, [
    { name: 'huge.jpg', mimeType: 'image/jpeg', buffer: big },
    { name: 'clip.webm', mimeType: 'video/webm', buffer: video },
  ]);

  const huge = bo.page.locator('.media-card', { hasText: 'huge.jpg' });
  await expect(huge.locator('.media-load-btn')).toBeVisible({ timeout: 15000 });
  await expect(huge.locator('.media-load-btn')).toContainText('17.0 MB');
  const movie = bo.page.locator('.media-card[data-media-kind="video"]');
  await expect(movie.locator('video.media-video')).toBeVisible({ timeout: 15000 });
  expect(await requestsSent(bo.page)).toBe(1);

  // While it loads: the same numbers as any file card (progress, speed, connections, Cancel).
  await ana.page.evaluate(() => window.__dchat.throttleUploads(10));
  await huge.locator('.media-load-btn').click();
  await expect(huge.locator('.file-progress-label')).toContainText(/Downloading: \d+% \(.+\/s/, { timeout: 15000 });
  await expect(huge.locator('.file-cancel-btn')).toBeVisible();
  await expect(huge.locator('.media-progress-label')).toHaveText(/^\d+%$/);
  await ana.page.evaluate(() => window.__dchat.throttleUploads(0));
  await expect(huge.locator('.media-image')).toBeVisible({ timeout: 60000 });
  await expect.poll(() => imageShown(huge.locator('.media-image'))).toBe(true);
  // Done: size, time and average speed, as on any file card.
  await expect(huge.locator('.file-summary')).toHaveText(/^17\.0 MB in \d+\.\d s · [\d.]+ (KB|MB)\/s average$/);

  // Play the video (muted: no gesture needed), then react to its message.
  await bo.page.evaluate(() => {
    const v = document.querySelector('video.media-video');
    v.__mark = 'same element';
    v.muted = true;
    v.loop = true;
    return v.play();
  });
  await expect.poll(() => bo.page.evaluate(() => document.querySelector('video.media-video').currentTime)).toBeGreaterThan(0.2);
  const row = bo.page.locator('.message-row', { has: movie });
  await row.hover();
  await row.locator('.react-btn').click();
  await row.locator('.reaction-option').first().click();
  await expect(row.locator('.reaction-chip')).toHaveCount(1);
  await expect(ana.page.locator('.message-row', { has: ana.page.locator('.media-card[data-media-kind="video"]') }).locator('.reaction-chip')).toHaveCount(1);
  const after = await bo.page.evaluate(() => {
    const v = document.querySelector('video.media-video');
    return { mark: v.__mark, paused: v.paused };
  });
  expect(after).toEqual({ mark: 'same element', paused: false });

  for (const m of [ana, bo]) await m.context.close();
});

test('SVG stays a plain file; a picture the browser cannot open keeps its card and saves from memory', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>');
  const fake = Buffer.from('not a picture at all');
  await shareMedia(ana.page, [
    { name: 'logo.svg', mimeType: 'image/svg+xml', buffer: svg },
    { name: 'fake.png', mimeType: 'image/png', buffer: fake },
  ]);
  for (const page of [ana.page, bo.page]) {
    await expect(page.locator('.file-card:not(.media-card)', { hasText: 'logo.svg' })).toBeVisible({ timeout: 15000 });
    // Loaded, failed to open: the card stays and says why.
    const broken = page.locator('.media-card', { hasText: 'fake.png' });
    await expect(broken.locator('.media-cant-play')).toContainText("This browser can't open it", { timeout: 15000 });
    await expect(broken.locator('.media-error-detail')).toHaveText('the picture could not be decoded (image/png, 20 B in memory, readable)');
    await expect(broken.locator('.media-image')).toHaveCount(0);
  }
  expect(await ana.page.locator('img[src^="blob:"]').count()).toBe(0);
  // Download saves the bytes already here: nothing is pulled again.
  const requests = await requestsSent(bo.page);
  const got = await saveVia(bo.page, bo.page.locator('.media-card', { hasText: 'fake.png' }).locator('.media-download-btn'));
  expect(got.bytes.equals(fake)).toBe(true);
  expect(await requestsSent(bo.page)).toBe(requests);

  for (const m of [ana, bo]) await m.context.close();
});

test('a video whose address stops working gets a fresh one and plays, again and again', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await shareMedia(ana.page, [{ name: 'clip.webm', mimeType: 'video/webm', buffer: await clip(ana.page, 1500) }]);
  const video = bo.page.locator('.media-card video.media-video');
  await expect(video).toBeVisible({ timeout: 15000 });
  // The browser can't read the address (Chrome: "MEDIA_ELEMENT_ERROR: Format error"), twice in
  // a row: each time the card retries with a fresh one a moment later.
  for (let round = 0; round < 2; round++) {
    const before = await video.getAttribute('src');
    await video.evaluate((v) => {
      URL.revokeObjectURL(v.src);
      v.load();
    });
    await expect.poll(() => video.getAttribute('src')).not.toBe(before);
    await expect.poll(() => video.evaluate((v) => v.readyState)).toBeGreaterThan(1);
  }
  await expect(bo.page.locator('.media-cant-play')).toHaveCount(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('past the memory budget the least recently viewed goes back to its thumbnail', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const first = await picture(ana.page, { width: 400, height: 300, type: 'image/jpeg', noise: true, color: '#f00' });
  const second = await picture(ana.page, { width: 400, height: 300, type: 'image/jpeg', noise: true, color: '#0f0' });
  await bo.page.evaluate((bytes) => window.__dchatMedia.mediaBudget(bytes), Math.max(first.length, second.length) + 1000);

  await shareMedia(ana.page, [{ name: 'first.jpg', mimeType: 'image/jpeg', buffer: first }]);
  const one = bo.page.locator('.media-card', { hasText: 'first.jpg' });
  await expect(one.locator('.media-image')).toBeVisible({ timeout: 15000 });
  await shareMedia(ana.page, [{ name: 'second.jpg', mimeType: 'image/jpeg', buffer: second }]);
  const two = bo.page.locator('.media-card', { hasText: 'second.jpg' });
  await expect(two.locator('.media-image')).toBeVisible({ timeout: 15000 });

  await expect(one.locator('.media-image')).toHaveCount(0);
  await expect(one.locator('.media-thumb')).toBeVisible();
  await expect(one.locator('.media-load-btn')).toBeVisible();
  expect(await heldMedia(bo.page)).toHaveLength(1);

  // A tap brings it back (and releases the other).
  await one.locator('.media-load-btn').click();
  await expect(one.locator('.media-image')).toBeVisible({ timeout: 15000 });
  await expect(two.locator('.media-image')).toHaveCount(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('a late joiner sees the thumbnail after the sender left', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await shareMedia(ana.page, [{ name: 'souvenir.png', mimeType: 'image/png', buffer: await picture(ana.page) }]);
  await expect(bo.page.locator('.media-card .media-image')).toBeVisible({ timeout: 15000 });
  const invite = inviteFrom(ana.page.url());
  // Gone without a word: away first, left once the grace (shortened here) runs out.
  await bo.page.evaluate(() => window.__dchat.awayGraceMs(2000));
  await ana.context.close();
  await expect(bo.page.locator('.member-row')).toHaveCount(1, { timeout: 30000 });

  const cy = await newMember(browser, 'Cy');
  await joinRoom(cy.page, invite, 'Cy');
  await expectDirectMesh(cy.page, ['Bo', 'Cy'], 'Cy');
  const late = cy.page.locator('.media-card', { hasText: 'souvenir.png' });
  await expect(late.locator('.media-thumb')).toBeVisible({ timeout: 20000 });
  await expect(late).toContainText('Sender left the room');
  await expect(late.locator('.media-load-btn')).toHaveCount(0);
  // Bo still has it, and can still save it.
  await expect(bo.page.locator('.media-card .media-image')).toBeVisible();
  await expect(bo.page.locator('.media-card .media-download-btn')).toBeVisible();

  for (const m of [bo, cy]) await m.context.close();
});

/** `jpeg` with an EXIF block (Orientation 6, and a GPS latitude) after its JFIF header. */
function withGps(jpeg) {
  const t = [];
  const u16 = (v) => t.push(v & 255, v >> 8);
  const u32 = (v) => t.push(v & 255, (v >> 8) & 255, (v >> 16) & 255, (v >>> 24) & 255);
  t.push(...Buffer.from('II*\0'));
  u32(8);
  u16(2);
  u16(0x0112); u16(3); u32(1); u32(6); // Orientation
  u16(0x8825); u16(4); u32(1); u32(38); u32(0); // GPS directory at 38
  u16(2);
  u16(1); u16(2); u32(2); t.push(...Buffer.from('N\0\0\0')); // GPSLatitudeRef
  u16(2); u16(5); u32(3); u32(68); u32(0); // GPSLatitude at 68
  for (const v of [48, 1, 51, 1, 2400, 100]) u32(v);
  const exif = Buffer.concat([Buffer.from('Exif\0\0'), Buffer.from(t)]);
  const app1 = Buffer.concat([Buffer.from([0xff, 0xe1, (exif.length + 2) >> 8, (exif.length + 2) & 255]), exif]);
  const afterJfif = 4 + jpeg.readUInt16BE(4);
  return Buffer.concat([jpeg.subarray(0, afterJfif), app1, jpeg.subarray(afterJfif)]);
}

test('a photo that records its location says so, and Remove location sends it without', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const photo = withGps(await picture(ana.page, { type: 'image/jpeg' }));
  const latitude = Buffer.from([0x60, 0x09, 0, 0]); // 2400, little-endian
  const orientation = Buffer.from([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0]);
  expect(photo.includes(latitude)).toBe(true);

  await ana.page.setInputFiles('#file-input-hidden', { name: 'trip.jpg', mimeType: 'image/jpeg', buffer: photo });
  const chip = ana.page.locator('.attachment-chip');
  await expect(chip.locator('.attachment-gps')).toContainText('This photo contains its location', { timeout: 15000 });
  await chip.locator('.btn-remove-location').click();
  await expect(chip.locator('.attachment-gps')).toHaveCount(0);
  await expect(ana.page.locator('.attachment-chip[data-gps="false"]')).toHaveCount(1);
  await ana.page.locator('footer.input-bar .send-btn').click();

  const boCard = bo.page.locator('.media-card', { hasText: 'trip.jpg' });
  await expect(boCard.locator('.media-image')).toBeVisible({ timeout: 15000 });
  const got = await saveVia(bo.page, boCard.locator('.media-download-btn'));
  expect(got.bytes.length).toBe(photo.length);
  expect(got.bytes.includes(latitude)).toBe(false);
  expect(got.bytes.includes(Buffer.from('N\0\0\0'))).toBe(false);
  expect(got.bytes.includes(orientation)).toBe(true);

  for (const m of [ana, bo]) await m.context.close();
});
