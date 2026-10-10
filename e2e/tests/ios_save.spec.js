import crypto from 'node:crypto';
import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember } from './helpers.js';

test.describe.configure({ timeout: 120000 });

const IPHONE_UA =
  'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1';

// iOS as dchat meets it: no save picker, and a share sheet for files. The stub records what
// was shared; `__shareMode = 'abort'` closes the sheet, `__canShareFiles = false` has none.
const IOS = `
  delete window.showSaveFilePicker;
  window.__shared = [];
  window.__shareCalls = 0;
  window.__shareMode = 'resolve';
  window.__canShareFiles = true;
  Object.defineProperty(navigator, 'canShare', {
    configurable: true,
    value: (data) => window.__canShareFiles && Array.isArray(data?.files) && data.files.length > 0,
  });
  Object.defineProperty(navigator, 'share', {
    configurable: true,
    value: (data) => {
      window.__shareCalls += 1;
      if (window.__shareMode === 'abort') return Promise.reject(new DOMException('Share canceled', 'AbortError'));
      window.__shared.push(data.files[0]);
      return Promise.resolve();
    },
  });
`;

/** A member on an iPhone: Safari's user agent and the share stub, counting downloads. */
async function iosMember(browser, label) {
  const context = await browser.newContext({ userAgent: IPHONE_UA });
  await context.addInitScript(IOS);
  const page = await context.newPage();
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.log(`${label} ERROR:`, msg.text());
  });
  const downloads = [];
  page.on('download', (download) => downloads.push(download));
  return { context, page, downloads };
}

/** Ana (desktop) shares; Bo is on an iPhone. */
async function room(browser) {
  const ana = await newMember(browser, 'Ana');
  const bo = await iosMember(browser, 'Bo');
  const admin = await createRoom(ana.page, { name: 'Ana' });
  await joinRoom(bo.page, inviteFrom(admin), 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  return { ana, bo };
}

async function shareFile(page, name, buffer, mimeType) {
  await page.setInputFiles('#file-input-hidden', { name, mimeType, buffer });
  await expect(page.locator('.attachment-chip')).toContainText(name);
  await page.locator('footer.input-bar .send-btn').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
}

/** Download on Bo's side and wait until it is ready to save. */
async function downloadToReady(bo, timeout = 15000) {
  const card = bo.page.locator('.file-card');
  await card.locator('.file-download-btn').click({ timeout: 10000 });
  await expect(card.locator('.file-save-btn')).toBeVisible({ timeout });
  return card;
}

/** Name, type and SHA-256 of the `index`-th file handed to the share sheet. */
async function shared(page, index = 0) {
  return page.evaluate(async (i) => {
    const file = window.__shared[i];
    const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
    const sha256 = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
    return { name: file.name, type: file.type, size: file.size, sha256 };
  }, index);
}

const sha256 = (buffer) => crypto.createHash('sha256').update(buffer).digest('hex');

test('a finished download waits for Save and goes to the share sheet, never out of the app', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  const content = Buffer.from('[.ShellClassInfo]\r\nIconResource=C:\\Windows\\System32\\imageres.dll,-3\r\n');
  await shareFile(ana.page, 'desktop.ini', content, 'application/octet-stream');

  const card = await downloadToReady(bo);
  await expect(card).toContainText('Ready to save');
  await expect(bo.page.locator('.toast')).toHaveText('Download finished: tap 💾 Save to keep it');
  // Nothing was handed to the system on its own.
  await bo.page.waitForTimeout(500);
  expect(bo.downloads).toHaveLength(0);
  expect(await bo.page.evaluate(() => window.__shareCalls)).toBe(0);

  await card.locator('.file-save-btn').click();
  await expect(card).toContainText('Download complete');
  await expect(card.locator('.file-save-btn')).toHaveCount(0);
  const file = await shared(bo.page);
  expect(file.name).toBe('desktop.ini');
  expect(file.sha256).toBe(sha256(content));
  expect(bo.downloads).toHaveLength(0);
  // Still in the room.
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  for (const m of [ana, bo]) await m.context.close();
});

test('closing the share sheet keeps Save; the next tap shares', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  const content = Buffer.from('notes for later');
  await shareFile(ana.page, 'notes.txt', content, 'text/plain');
  const card = await downloadToReady(bo);

  await bo.page.evaluate(() => { window.__shareMode = 'abort'; });
  await card.locator('.file-save-btn').click();
  await expect.poll(() => bo.page.evaluate(() => window.__shareCalls)).toBe(1);
  await bo.page.waitForTimeout(300);
  await expect(card).toContainText('Ready to save');
  await expect(card.locator('.file-save-btn')).toBeVisible();

  await bo.page.evaluate(() => { window.__shareMode = 'resolve'; });
  await card.locator('.file-save-btn').click();
  await expect(card).toContainText('Download complete');
  const file = await shared(bo.page);
  expect(file.name).toBe('notes.txt');
  expect(file.type).toBe('text/plain');
  expect(file.sha256).toBe(sha256(content));

  for (const m of [ana, bo]) await m.context.close();
});

test('without a share sheet for files, Save falls back to a download', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  await bo.page.evaluate(() => { window.__canShareFiles = false; });
  const content = Buffer.from('fallback bytes');
  await shareFile(ana.page, 'fallback.txt', content, 'text/plain');
  const card = await downloadToReady(bo);
  expect(bo.downloads).toHaveLength(0);

  const downloadPromise = bo.page.waitForEvent('download');
  await card.locator('.file-save-btn').click();
  const download = await downloadPromise;
  const chunks = [];
  for await (const chunk of await download.createReadStream()) chunks.push(chunk);
  expect(download.suggestedFilename()).toBe('fallback.txt');
  expect(sha256(Buffer.concat(chunks))).toBe(sha256(content));
  await expect(card).toContainText('Download complete');
  expect(await bo.page.evaluate(() => window.__shareCalls)).toBe(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('a file ready to save survives a rotated link', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  const content = Buffer.from('kept across the move');
  await shareFile(ana.page, 'kept.txt', content, 'text/plain');
  const card = await downloadToReady(bo);

  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  await expect(bo.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  await expect(card).toContainText('Ready to save');
  await card.locator('.file-save-btn').click();
  await expect(card).toContainText('Download complete');
  const file = await shared(bo.page);
  expect(file.name).toBe('kept.txt');
  expect(file.sha256).toBe(sha256(content));

  for (const m of [ana, bo]) await m.context.close();
});

test('a 20 MB file is folded as it arrives and shared byte-exact', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  // Not one repeated byte: a dropped, doubled or reordered chunk changes the hash.
  const content = crypto.randomBytes(20 * 1024 * 1024 + 12345);
  await shareFile(ana.page, 'backup.bin', content, 'application/octet-stream');
  const card = await downloadToReady(bo, 90000);

  await card.locator('.file-save-btn').click();
  await expect(card).toContainText('Download complete');
  const file = await shared(bo.page);
  expect(file.name).toBe('backup.bin');
  expect(file.type).toBe('application/octet-stream');
  expect(file.size).toBe(content.length);
  expect(file.sha256).toBe(sha256(content));
  expect(bo.downloads).toHaveLength(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('a photo shows in the chat by itself; Download hands the copy already here to the share sheet', async ({ browser }) => {
  const { ana, bo } = await room(browser);
  const b64 = await ana.page.evaluate(() => {
    const canvas = document.createElement('canvas');
    canvas.width = 200;
    canvas.height = 150;
    canvas.getContext('2d').fillRect(20, 20, 100, 80);
    return canvas.toDataURL('image/png').split(',')[1];
  });
  const photo = Buffer.from(b64, 'base64');
  await shareFile(ana.page, 'snap.png', photo, 'image/png');

  const card = bo.page.locator('.media-card');
  await expect(card.locator('.media-image')).toBeVisible({ timeout: 15000 });
  // Viewing hands nothing to the system.
  await bo.page.waitForTimeout(500);
  expect(bo.downloads).toHaveLength(0);
  expect(await bo.page.evaluate(() => window.__shareCalls)).toBe(0);

  await card.locator('.media-download-btn').click();
  await expect.poll(() => bo.page.evaluate(() => window.__shared.length)).toBe(1);
  const file = await shared(bo.page);
  expect(file.name).toBe('snap.png');
  expect(file.type).toBe('image/png');
  expect(file.sha256).toBe(sha256(photo));
  expect(bo.downloads).toHaveLength(0);

  for (const m of [ana, bo]) await m.context.close();
});
