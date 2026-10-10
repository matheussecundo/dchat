import { test, expect } from '@playwright/test';
import { twoMembers } from './helpers.js';

test.describe.configure({ timeout: 120000 });

/**
 * Drag `files` ([name, type, text]) or `text` over the page and (unless `drop: false`) drop
 * them, as the browser would; returns whether the page prevented the drop's default (the
 * browser opening the file in place of the room).
 */
function drag(page, { files = [], text = null, drop = true, target = 'main.chat-container' }) {
  return page.evaluate(({ files, text, drop, target }) => {
    const transfer = new DataTransfer();
    for (const [name, type, body] of files) transfer.items.add(new File([body], name, { type }));
    if (text !== null) transfer.setData('text/plain', text);
    const at = document.querySelector(target);
    const fire = (type) => {
      const ev = new DragEvent(type, { dataTransfer: transfer, bubbles: true, cancelable: true });
      at.dispatchEvent(ev);
      return ev.defaultPrevented;
    };
    fire('dragenter');
    fire('dragover');
    return drop ? fire('drop') : null;
  }, { files, text, drop, target });
}

const chips = (page) => page.locator('.attachment-chip');

test('files dropped on the room are staged together and sent as one card each, caption on the first', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);

  // The overlay shows while files are dragged over the room.
  await drag(ana.page, { files: [['a.txt', 'text/plain', 'one']], drop: false });
  await expect(ana.page.locator('.drop-overlay')).toBeVisible();
  await ana.page.evaluate(() => {
    const transfer = new DataTransfer();
    transfer.items.add(new File(['x'], 'x.txt'));
    document.querySelector('main.chat-container').dispatchEvent(new DragEvent('dragleave', { dataTransfer: transfer, bubbles: true }));
  });
  await expect(ana.page.locator('.drop-overlay')).toHaveCount(0);

  const prevented = await drag(ana.page, {
    files: [
      ['notes-1.txt', 'text/plain', 'first file'],
      ['notes-2.txt', 'text/plain', 'second file'],
      ['notes-3.txt', 'text/plain', 'third file'],
    ],
  });
  expect(prevented).toBe(true);
  await expect(ana.page.locator('.drop-overlay')).toHaveCount(0);
  await expect(chips(ana.page)).toHaveCount(3);
  // One more from 📎 joins them.
  await ana.page.setInputFiles('#file-input-hidden', { name: 'notes-4.txt', mimeType: 'text/plain', buffer: Buffer.from('fourth') });
  await expect(chips(ana.page)).toHaveCount(4);
  await chips(ana.page).nth(3).locator('.btn-remove-attachment').click();
  await expect(chips(ana.page)).toHaveCount(3);

  await ana.page.locator('footer.input-bar input').fill('Three notes');
  await ana.page.locator('footer.input-bar .send-btn').click();
  await expect(chips(ana.page)).toHaveCount(0);

  const cards = bo.page.locator('.file-card');
  await expect(cards).toHaveCount(3, { timeout: 15000 });
  for (let i = 0; i < 3; i++) await expect(cards.nth(i)).toContainText(`notes-${i + 1}.txt`);
  await expect(cards.nth(0).locator('.file-caption')).toHaveText('Three notes');
  await expect(bo.page.locator('.file-caption')).toHaveCount(1);

  for (const m of [ana, bo]) await m.context.close();
});

test('at most 10 files wait to be sent; text drags and stray drops change nothing', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const eleven = Array.from({ length: 11 }, (_, i) => [`f${i}.txt`, 'text/plain', `file ${i}`]);
  await drag(ana.page, { files: eleven });
  await expect(chips(ana.page)).toHaveCount(10);
  await expect(ana.page.locator('.toast')).toHaveText('Up to 10 files at a time');
  await ana.page.setInputFiles('#file-input-hidden', { name: 'more.txt', mimeType: 'text/plain', buffer: Buffer.from('x') });
  await expect(chips(ana.page)).toHaveCount(10);

  // Dragged text is not a file: no overlay, nothing staged.
  const before = ana.page.url();
  await drag(bo.page, { text: 'just words' });
  await expect(bo.page.locator('.drop-overlay')).toHaveCount(0);
  await expect(chips(bo.page)).toHaveCount(0);
  // A file dropped anywhere in the room (here on the header) never leaves the room.
  expect(await drag(ana.page, { files: [['stray.txt', 'text/plain', 'x']], target: 'header' })).toBe(true);
  expect(ana.page.url()).toBe(before);

  for (const m of [ana, bo]) await m.context.close();
});

test('a pasted screenshot is staged under a dated name and shows in the chat', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  const input = ana.page.locator('footer.input-bar input');
  await input.focus();
  await ana.page.evaluate(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 64;
    canvas.height = 48;
    canvas.getContext('2d').fillRect(0, 0, 64, 48);
    const blob = await new Promise((resolve) => canvas.toBlob(resolve, 'image/png'));
    const transfer = new DataTransfer();
    transfer.items.add(new File([blob], 'image.png', { type: 'image/png' }));
    document.querySelector('footer.input-bar input').dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true, cancelable: true }));
  });
  await expect(chips(ana.page)).toHaveCount(1);
  await expect(chips(ana.page).locator('.attachment-name')).toHaveText(/^paste-\d{8}-\d{6}\.png$/);
  await expect(chips(ana.page).locator('.attachment-thumb')).toBeVisible({ timeout: 10000 });
  await expect(input).toHaveValue('');

  await ana.page.locator('footer.input-bar .send-btn').click();
  await expect(bo.page.locator('.media-card .media-image')).toBeVisible({ timeout: 15000 });

  for (const m of [ana, bo]) await m.context.close();
});

test('while recording a voice message, files cannot be dropped', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await ana.page.locator('footer.input-bar .record-btn').click();
  await expect(ana.page.locator('.recorder-panel')).toBeVisible({ timeout: 10000 });
  expect(await drag(ana.page, { files: [['late.txt', 'text/plain', 'x']] })).toBe(true);
  await expect(ana.page.locator('.drop-overlay')).toHaveCount(0);
  await expect(chips(ana.page)).toHaveCount(0);
  await ana.page.locator('.rec-cancel').click();
  await expect(ana.page.locator('.recorder-panel')).toHaveCount(0);

  for (const m of [ana, bo]) await m.context.close();
});
