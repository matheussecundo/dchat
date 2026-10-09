import { test, expect } from '@playwright/test';
import {
  createRoom,
  expectDirectMesh,
  expectNoStorage,
  inviteFrom,
  joinRoom,
  memberRow,
  newMember,
  sendMessage,
} from './helpers.js';

test.describe.configure({ timeout: 120000 });

async function room(browser, names) {
  const members = [];
  for (const name of names) members.push(await newMember(browser, name));
  const invite = inviteFrom(await createRoom(members[0].page, { name: names[0] }));
  for (let i = 1; i < names.length; i++) await joinRoom(members[i].page, invite, names[i]);
  for (let i = 0; i < names.length; i++) await expectDirectMesh(members[i].page, names, names[i]);
  return members;
}

async function shareFile(page, name, content, caption) {
  await page.setInputFiles('#file-input-hidden', { name, mimeType: 'application/pdf', buffer: Buffer.from(content) });
  await expect(page.locator('.attachment-chip')).toContainText(name);
  if (caption) await page.locator('footer.input-bar input').fill(caption);
  await page.locator('footer.input-bar .send-btn').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
}

/** Click Download and return the downloaded bytes (in-memory Blob fallback). */
async function downloadVia(page, card) {
  await page.evaluate(() => { delete window.showSaveFilePicker; });
  const downloadPromise = page.waitForEvent('download');
  await card.locator('.file-download-btn').click();
  const download = await downloadPromise;
  const chunks = [];
  for await (const chunk of await download.createReadStream()) chunks.push(chunk);
  return { name: download.suggestedFilename(), text: Buffer.concat(chunks).toString('utf-8') };
}

test('room-wide file card: each member pulls it directly, byte-exact, and the author sees the tally', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);

  // Staging chip can be removed before sending.
  await ana.page.setInputFiles('#file-input-hidden', { name: 'draft.txt', mimeType: 'text/plain', buffer: Buffer.from('x') });
  await ana.page.locator('.attachment-chip button.btn-remove-attachment').click();
  await expect(ana.page.locator('.attachment-chip')).toHaveCount(0);

  // 150 KB = 3 encrypted 64 KB chunks.
  const fileName = 'confidential_report.pdf';
  const content = 'Zero-Knowledge Confidential Report Header\n' + 'A'.repeat(150000) + '\nReport Footer';
  await shareFile(ana.page, fileName, content, 'Audit document for both of you.');

  const anaCard = ana.page.locator('.file-card');
  await expect(anaCard).toContainText('Shared with the room');
  await expect(anaCard).toContainText('146.5 KB');
  for (const m of [bo, cy]) {
    const card = m.page.locator('.file-card');
    await expect(card).toContainText(fileName, { timeout: 10000 });
    await expect(m.page.locator('.chat-container')).toContainText('Audit document for both of you.');
    await expect(card.locator('.file-download-btn')).toBeVisible();
  }

  for (const m of [bo, cy]) {
    const got = await downloadVia(m.page, m.page.locator('.file-card'));
    expect(got.name).toBe(fileName);
    expect(got.text).toBe(content);
    await expect(m.page.locator('.file-card')).toContainText('Download complete');
  }
  await expect(anaCard.locator('.file-count-done')).toHaveText('Received: 2', { timeout: 10000 });
  await expect(anaCard.locator('.file-count-active')).toHaveText('Sending: 0');

  for (const m of [ana, bo, cy]) await expectNoStorage(m.page);
  await bo.page.reload();
  await expect(bo.page.locator('.file-card')).toHaveCount(0);

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('an offer has only a Download button; withdrawing it reaches everyone', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);
  await shareFile(ana.page, 'plans.pdf', 'secret plans', null);
  for (const m of [bo, cy]) {
    await expect(m.page.locator('.file-card .file-download-btn')).toBeVisible({ timeout: 10000 });
    await expect(m.page.locator('.file-card button')).toHaveCount(1);
  }

  await ana.page.locator('.file-card .file-withdraw-btn').click();
  await expect(ana.page.locator('.file-card')).toContainText('Withdrawn by sender');
  for (const m of [bo, cy]) {
    await expect(m.page.locator('.file-card')).toContainText('Withdrawn by sender', { timeout: 10000 });
    await expect(m.page.locator('.file-download-btn')).toHaveCount(0);
  }

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a cancelled download can be started again', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await ana.page.evaluate(() => window.__dchat.throttleUploads(400));
  const content = 'R'.repeat(5 * 65536) + 'end'; // 6 chunks, ~2.4 s when throttled
  await shareFile(ana.page, 'retry.bin', content, null);

  await bo.page.evaluate(() => { delete window.showSaveFilePicker; });
  await bo.page.locator('.file-download-btn').click({ timeout: 10000 });
  await expect(bo.page.locator('.file-progress-label')).toBeVisible({ timeout: 10000 });
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 1', { timeout: 10000 });
  await bo.page.locator('.file-card .file-cancel-btn').click();
  await expect(bo.page.locator('.file-card')).toContainText('Cancelled');

  // Asked again at once, while Ana's cancelled run still sleeps between chunks.
  const got = await downloadVia(bo.page, bo.page.locator('.file-card'));
  expect(got.name).toBe('retry.bin');
  expect(got.text).toBe(content);
  await expect(bo.page.locator('.file-card')).toContainText('Download complete');
  await expect(ana.page.locator('.file-count-done')).toHaveText('Received: 1', { timeout: 10000 });
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 0');

  for (const m of [ana, bo]) await m.context.close();
});

test('a finished download shows its time and speed, and can be downloaded again', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  const content = 'S'.repeat(150000);
  await shareFile(ana.page, 'again.bin', content, null);
  const card = bo.page.locator('.file-card');
  expect((await downloadVia(bo.page, card)).text).toBe(content);
  await expect(card).toContainText('Download complete');
  await expect(card.locator('.file-summary')).toHaveText(/^146\.5 KB in \d+\.\d s · [\d.]+ (KB|MB)\/s average$/);

  const download = bo.page.waitForEvent('download');
  await card.locator('.file-download-again-btn').click();
  const chunks = [];
  for await (const chunk of await (await download).createReadStream()) chunks.push(chunk);
  expect(Buffer.concat(chunks).toString('utf-8')).toBe(content);
  await expect(card).toContainText('Download complete');
  await expect(ana.page.locator('.file-count-done')).toHaveText('Received: 2', { timeout: 10000 });

  // Withdrawn: the card stays finished, but the file can't be downloaded again.
  await ana.page.locator('.file-card .file-withdraw-btn').click();
  await expect(card.locator('.file-download-again-btn')).toHaveCount(0, { timeout: 10000 });
  await expect(card).toContainText('Download complete');
  await expect(card.locator('.file-summary')).toBeVisible();

  for (const m of [ana, bo]) await m.context.close();
});

/** `chunks` 64 KB chunks, each filled with its own number: any chunk out of place shows. */
function numberedChunks(chunks) {
  return Array.from({ length: chunks }, (_, i) => String(i).padStart(4, '0').repeat(16384)).join('');
}

/** Click Download and return the promise of the downloaded text (in-memory Blob fallback). */
async function startDownload(page) {
  await page.evaluate(() => { delete window.showSaveFilePicker; });
  const downloadPromise = page.waitForEvent('download', { timeout: 60000 });
  await page.locator('.file-card .file-download-btn').click({ timeout: 10000 });
  return downloadPromise.then(async (download) => {
    const chunks = [];
    for await (const chunk of await download.createReadStream()) chunks.push(chunk);
    return Buffer.concat(chunks).toString('utf-8');
  });
}

test('a download spreads over extra file links and arrives in order, byte-exact', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await ana.page.evaluate(() => { window.__dchat.forceFileLinks(3); window.__dchat.throttleUploads(40); });
  const content = numberedChunks(64);
  await shareFile(ana.page, 'striped.bin', content, null);

  const text = startDownload(bo.page);
  // The main link and three file links, on both ends.
  await expect(bo.page.locator('.file-progress-label')).toContainText('4 connections', { timeout: 10000 });
  await expect(ana.page.locator('.file-peer-stats')).toContainText('4 connections');
  expect(await text).toBe(content);
  await expect(bo.page.locator('.file-card')).toContainText('Download complete');
  expect(await bo.page.evaluate(() => window.__dchat.fileLinkChunks())).toBeGreaterThan(10);
  await expect(ana.page.locator('.file-count-done')).toHaveText('Received: 1', { timeout: 10000 });
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 0');
  expect(await ana.page.evaluate(() => window.__dchat.openFileLinks())).toBe(3);

  for (const m of [ana, bo]) await m.context.close();
});

test('chunks lost with a file link are sent again', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await ana.page.evaluate(() => { window.__dchat.forceFileLinks(1); window.__dchat.throttleUploads(20); });
  // What Bo receives over the file link vanishes, as if the link died with it.
  await bo.page.evaluate(() => window.__dchat.discardFileLinkChunks(true));
  const content = numberedChunks(64);
  await shareFile(ana.page, 'resent.bin', content, null);

  const text = startDownload(bo.page);
  await expect.poll(() => bo.page.evaluate(() => window.__dchat.fileLinkChunks()), { timeout: 10000 }).toBeGreaterThan(3);
  // Bo's download waits at the first missing chunk until Ana loses the link.
  expect(await ana.page.evaluate(() => window.__dchat.dropFileLink())).toBe(true);
  expect(await text).toBe(content);
  await expect(ana.page.locator('.file-count-done')).toHaveText('Received: 1', { timeout: 10000 });
  await expect.poll(() => bo.page.evaluate(() => window.__dchat.openFileLinks()), { timeout: 10000 }).toBe(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('progress updates leave the cards and their buttons in place', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await ana.page.evaluate(() => window.__dchat.throttleUploads(100));
  await shareFile(ana.page, 'steady.bin', 'B'.repeat(40 * 65536), null); // ~4 s when throttled
  await bo.page.evaluate(() => { delete window.showSaveFilePicker; });
  await bo.page.locator('.file-download-btn').click({ timeout: 10000 });

  const label = bo.page.locator('.file-progress-label');
  await expect(label).toContainText(/Downloading: [1-9]/, { timeout: 10000 });
  await expect(ana.page.locator('.file-peer-stats')).toBeVisible();
  const cancel = await bo.page.locator('.file-card .file-cancel-btn').elementHandle();
  const withdraw = await ana.page.locator('.file-card .file-withdraw-btn').elementHandle();
  const before = await label.textContent();
  await expect(label).not.toHaveText(before);
  // Several updates later, the buttons under the pointer are still the same elements.
  await bo.page.waitForTimeout(600);
  expect(await cancel.evaluate((el) => el.isConnected)).toBe(true);
  expect(await withdraw.evaluate((el) => el.isConnected)).toBe(true);
  await cancel.click();
  await expect(bo.page.locator('.file-card')).toContainText('Cancelled');

  for (const m of [ana, bo]) await m.context.close();
});

/** Pick the connections cap in ⚙️ Settings. */
async function setConnectionsCap(page, cap) {
  await page.locator('#audio-settings-btn').click();
  await page.locator('#file-connections-select').selectOption(String(cap));
  await page.locator('.audio-settings-modal .modal-title-row button').click();
}

test('the connections cap applies at once, from either side', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await ana.page.evaluate(() => { window.__dchat.forceFileLinks(3); window.__dchat.throttleUploads(40); });
  const content = numberedChunks(96);
  await shareFile(ana.page, 'capped.bin', content, null);

  // The sender lowers its cap mid-transfer: both cards drop to one connection.
  const text = startDownload(bo.page);
  const label = bo.page.locator('.file-progress-label');
  await expect(label).toContainText('4 connections', { timeout: 10000 });
  await setConnectionsCap(ana.page, 1);
  await expect(ana.page.locator('.file-peer-stats')).not.toContainText('connections', { timeout: 5000 });
  await expect(label).not.toContainText('connections', { timeout: 5000 });
  expect(await text).toBe(content);

  // The downloader caps at one: the links it had close, and new ones are refused.
  await setConnectionsCap(ana.page, 8);
  await setConnectionsCap(bo.page, 1);
  await expect.poll(() => bo.page.evaluate(() => window.__dchat.openFileLinks()), { timeout: 10000 }).toBe(0);
  const viaLinks = await bo.page.evaluate(() => window.__dchat.fileLinkChunks());
  await shareFile(ana.page, 'capped2.bin', content, null);
  const download = bo.page.waitForEvent('download', { timeout: 60000 });
  await bo.page.locator('.file-card').last().locator('.file-download-btn').click({ timeout: 10000 });
  const chunks = [];
  for await (const chunk of await (await download).createReadStream()) chunks.push(chunk);
  expect(Buffer.concat(chunks).toString('utf-8')).toBe(content);
  expect(await bo.page.evaluate(() => window.__dchat.fileLinkChunks())).toBe(viaLinks);
  expect(await bo.page.evaluate(() => window.__dchat.openFileLinks())).toBe(0);

  for (const m of [ana, bo]) await m.context.close();
});

test('uploads add file links on their own, up to the cap in the settings', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  // 512 chunks at no more than 3.2 MB/s: long enough for the first decision, at 3 s.
  await ana.page.evaluate(() => window.__dchat.throttleUploads(20));
  await shareFile(ana.page, 'long.bin', 'L'.repeat(512 * 65536), null);
  await bo.page.evaluate(() => { delete window.showSaveFilePicker; });

  // Capped at one connection: never more.
  await ana.page.locator('#audio-settings-btn').click();
  await ana.page.locator('#file-connections-select').selectOption('1');
  await ana.page.locator('.audio-settings-modal .modal-title-row button').click();
  await bo.page.locator('.file-download-btn').click({ timeout: 10000 });
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 1', { timeout: 10000 });
  await ana.page.waitForTimeout(5000);
  expect(await ana.page.evaluate(() => window.__dchat.openFileLinks())).toBe(0);
  await bo.page.locator('.file-card .file-cancel-btn').click();
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 0', { timeout: 10000 });

  // Up to eight: after warming up, the upload tries a file link.
  await ana.page.locator('#audio-settings-btn').click();
  await ana.page.locator('#file-connections-select').selectOption('8');
  await ana.page.locator('.audio-settings-modal .modal-title-row button').click();
  await bo.page.locator('.file-download-btn').click();
  await expect.poll(() => ana.page.evaluate(() => window.__dchat.openFileLinks()), { timeout: 10000 }).toBeGreaterThan(0);
  await bo.page.locator('.file-card .file-cancel-btn').click();

  for (const m of [ana, bo]) await m.context.close();
});

test('uploads run two at a time; the third requester waits its turn', async ({ browser }) => {
  const [ana, bo, cy, dee] = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  await ana.page.evaluate(() => window.__dchat.throttleUploads(400));
  const content = 'Q'.repeat(5 * 65536); // 5 chunks, ~2 s per upload when throttled
  await shareFile(ana.page, 'queue.bin', content, null);

  for (const m of [bo, cy, dee]) {
    await m.page.evaluate(() => { delete window.showSaveFilePicker; });
    await expect(m.page.locator('.file-download-btn')).toBeVisible({ timeout: 10000 });
  }
  const downloads = [bo, cy, dee].map((m) => m.page.waitForEvent('download', { timeout: 60000 }));
  await bo.page.locator('.file-download-btn').click();
  await cy.page.locator('.file-download-btn').click();
  await expect(ana.page.locator('.file-count-active')).toHaveText('Sending: 2', { timeout: 10000 });
  await dee.page.locator('.file-download-btn').click();

  await expect(dee.page.locator('.file-queued')).toHaveText('⏳ Queued (#1)', { timeout: 10000 });
  await expect(ana.page.locator('.file-count-waiting')).toHaveText('Waiting: 1');

  for (const d of downloads) await d;
  await expect(dee.page.locator('.file-card')).toContainText('Download complete', { timeout: 30000 });
  await expect(ana.page.locator('.file-count-done')).toHaveText('Received: 3', { timeout: 10000 });

  for (const m of [ana, bo, cy, dee]) await m.context.close();
});

test('files need a direct link, and a transfer stops when the sender leaves', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);

  // Ana and Cy cannot connect directly: Cy sees the card but cannot download it.
  const anaKey = await ana.page.evaluate(() => window.__dchat.selfPubkey());
  const cyKey = await cy.page.evaluate(() => window.__dchat.selfPubkey());
  await ana.page.evaluate((pk) => window.__dchat.blockPeer(pk), cyKey);
  await cy.page.evaluate((pk) => window.__dchat.blockPeer(pk), anaKey);
  await expect(memberRow(cy.page, 'Ana')).toHaveAttribute('data-link', 'via', { timeout: 15000 });

  await ana.page.evaluate(() => window.__dchat.throttleUploads(500));
  await shareFile(ana.page, 'big.bin', 'Z'.repeat(10 * 65536), null);
  await expect(cy.page.locator('.file-card .file-unreachable')).toHaveText('Sender not directly reachable', { timeout: 10000 });
  await expect(cy.page.locator('.file-download-btn')).toHaveCount(0);

  // Bo starts downloading; Ana leaves mid-transfer.
  await bo.page.evaluate(() => { delete window.showSaveFilePicker; });
  await bo.page.locator('.file-download-btn').click({ timeout: 10000 });
  await expect(bo.page.locator('.file-progress-label')).toBeVisible({ timeout: 10000 });
  await ana.context.close();
  await expect(bo.page.locator('.file-card')).toContainText(/Interrupted|Sender left the room/, { timeout: 20000 });
  await expect(cy.page.locator('.file-card')).toContainText('Sender left the room', { timeout: 20000 });

  for (const m of [bo, cy]) await m.context.close();
});
