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

test('decline is local; withdrawing an offer reaches everyone', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);
  await shareFile(ana.page, 'plans.pdf', 'secret plans', null);

  await bo.page.locator('.file-card .file-decline-btn').click({ timeout: 10000 });
  await expect(bo.page.locator('.file-card')).toContainText('Declined');
  await expect(ana.page.locator('.file-card')).toContainText('Shared with the room');

  await ana.page.locator('.file-card .file-withdraw-btn').click();
  await expect(ana.page.locator('.file-card')).toContainText('Withdrawn by sender');
  await expect(cy.page.locator('.file-card')).toContainText('Withdrawn by sender', { timeout: 10000 });
  await expect(cy.page.locator('.file-download-btn')).toHaveCount(0);

  for (const m of [ana, bo, cy]) await m.context.close();
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
