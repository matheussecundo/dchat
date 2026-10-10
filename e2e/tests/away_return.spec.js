import { test, expect } from '@playwright/test';
import {
  createRoom,
  expectAudioFrom,
  expectDirectMesh,
  expectNoStorage,
  inviteFrom,
  joinRoom,
  joinVoice,
  memberRow,
  newMember,
  sendMessage,
  shareFile,
} from './helpers.js';

// A phone that switches apps freezes the tab: its links die without a word. `simulateFreeze`
// does that (links closed, signals and ticks ignored), `simulateResume(ms)` comes back as the
// page shown again would, after a pause of `ms`.
test.describe.configure({ timeout: 150000 });

const notice = (page, text) => page.locator('.system-notice', { hasText: text });
const selfKey = (page) => page.evaluate(() => window.__dchat.selfPubkey());
const keyState = (page) => page.evaluate(() => window.__dchat.adminKeyState());
const freeze = (member) => member.page.evaluate(() => window.__dchat.simulateFreeze());
const resume = (member, pausedMs = 60000) => member.page.evaluate((ms) => window.__dchat.simulateResume(ms), pausedMs);
const grace = (member, ms) => member.page.evaluate((v) => window.__dchat.awayGraceMs(v), ms);

/** Members join one at a time, so the creator sees them in this order (their seniority). */
async function room(browser, names) {
  const members = [];
  for (const name of names) members.push(await newMember(browser, name));
  const adminUrl = await createRoom(members[0].page, { name: names[0] });
  const invite = inviteFrom(adminUrl);
  for (let i = 1; i < names.length; i++) {
    await joinRoom(members[i].page, invite, names[i]);
    await expectDirectMesh(members[0].page, names.slice(0, i + 1), names[0]);
  }
  for (let i = 0; i < names.length; i++) await expectDirectMesh(members[i].page, names, names[i]);
  return members;
}

test('a member who switches apps stays listed as away and comes back as themselves', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);
  const cyKey = await selfKey(cy.page);
  await sendMessage(cy.page, 'before the call');
  await expect(ana.page.locator('.chat-container')).toContainText('before the call', { timeout: 10000 });

  // Cy's arrival was announced once; nothing more may be.
  const joined = await Promise.all([ana, bo].map((m) => notice(m.page, 'Cy joined').count()));
  await freeze(cy);
  for (const m of [ana, bo]) await expect(memberRow(m.page, 'Cy')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await sendMessage(ana.page, 'while you were out');

  await resume(cy);
  // Back at once (no retry backoff), under the same key, without a left/joined line anywhere.
  const back = Date.now();
  for (const m of [ana, bo]) await expect(memberRow(m.page, 'Cy')).toHaveAttribute('data-link', 'direct', { timeout: 15000 });
  expect(Date.now() - back).toBeLessThan(15000);
  await expectDirectMesh(cy.page, ['Ana', 'Bo', 'Cy'], 'Cy');
  expect(await selfKey(cy.page)).toBe(cyKey);
  await cy.page.waitForTimeout(2000);
  for (const [i, m] of [ana, bo].entries()) {
    await expect(notice(m.page, 'Cy left')).toHaveCount(0);
    await expect(notice(m.page, 'Cy joined')).toHaveCount(joined[i]);
  }
  // What was said meanwhile reaches Cy, and Cy still owns their earlier message.
  await expect(cy.page.locator('.chat-container')).toContainText('while you were out', { timeout: 15000 });
  await cy.page.locator('.message-row', { hasText: 'before the call' }).locator('.edit-btn').click();
  await cy.page.locator('footer.input-bar input').fill('before the call (edited)');
  await cy.page.locator('footer.input-bar input').press('Enter');
  await expect(ana.page.locator('.chat-container')).toContainText('before the call (edited)', { timeout: 10000 });

  for (const m of [ana, bo, cy]) await expectNoStorage(m.page);
  for (const m of [ana, bo, cy]) await m.context.close();
});

test('away past the grace counts as left; coming back later is a join', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await grace(ana, 3000);

  await freeze(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await expect(notice(ana.page, 'Bo left')).toBeVisible({ timeout: 10000 });
  await expect(ana.page.locator('.member-row')).toHaveCount(1);

  await resume(bo, 600000);
  await expect(notice(ana.page, 'Bo joined')).toBeVisible({ timeout: 15000 });
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');

  for (const m of [ana, bo]) await m.context.close();
});

/** Start downloading Ana's file on `page` (in-memory), returning the awaited download. */
async function startDownload(page) {
  await page.evaluate(() => { delete window.showSaveFilePicker; });
  const download = page.waitForEvent('download', { timeout: 90000 });
  await page.locator('.file-card .file-download-btn').click({ timeout: 10000 });
  return download;
}

async function bytesOf(download) {
  const chunks = [];
  for await (const chunk of await download.createReadStream()) chunks.push(chunk);
  return Buffer.concat(chunks);
}

/** A file of `chunks` 64 KB chunks whose every chunk differs, so a wrong resume shows. */
function patterned(chunks) {
  const buffer = Buffer.alloc(chunks * 65536);
  for (let i = 0; i < buffer.length; i += 4) buffer.writeUInt32BE(i / 4, i);
  return buffer;
}

async function shareBinary(page, name, buffer) {
  await page.setInputFiles('#file-input-hidden', { name, mimeType: 'application/octet-stream', buffer });
  await page.locator('footer.input-bar .send-btn').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
}

for (const who of ['sender', 'downloader']) {
  test(`a download pauses while the ${who} is away and resumes where it stopped, byte-exact`, async ({ browser }) => {
    const [ana, bo] = await room(browser, ['Ana', 'Bo']);
    const resumedFrom = [];
    bo.page.on('console', (msg) => {
      const at = msg.text().match(/Resuming a download from chunk (\d+)/);
      if (at) resumedFrom.push(Number(at[1]));
    });
    const content = patterned(48);
    await ana.page.evaluate(() => window.__dchat.throttleUploads(120));
    await shareBinary(ana.page, 'movie.bin', content);
    await expect(bo.page.locator('.file-card .file-download-btn')).toBeVisible({ timeout: 10000 });
    const download = startDownload(bo.page);
    await expect(bo.page.locator('.file-progress-label')).toContainText(/[1-9]\d?%/, { timeout: 20000 });

    const away = who === 'sender' ? ana : bo;
    await freeze(away);
    if (who === 'sender') {
      await expect(bo.page.locator('.file-card .file-paused-label')).toContainText('Paused: waiting for Ana', { timeout: 20000 });
    }
    await bo.page.waitForTimeout(2000);
    await resume(away);

    const got = await bytesOf(await download);
    expect(got.equals(content)).toBe(true);
    await expect(bo.page.locator('.file-card')).toContainText('Download complete');
    expect(resumedFrom.length).toBeGreaterThan(0);
    expect(resumedFrom[0]).toBeGreaterThan(0);
    // One request from the card; the resume is not a new download.
    expect(await bo.page.evaluate(() => window.__dchat.fileRequestsSent())).toBe(1);

    for (const m of [ana, bo]) await m.context.close();
  });
}

test('a file card waits for its sender while away, and ends once they count as left', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await shareFile(ana.page, 'notes.pdf', 'some notes', null);
  await expect(bo.page.locator('.file-card .file-download-btn')).toBeVisible({ timeout: 10000 });

  await freeze(ana);
  await expect(bo.page.locator('.file-card .file-sender-away')).toHaveText('Available when Ana is back', { timeout: 20000 });
  await resume(ana);
  await expect(bo.page.locator('.file-card .file-download-btn')).toBeVisible({ timeout: 15000 });

  await grace(bo, 2000);
  await freeze(ana);
  await expect(bo.page.locator('.file-card')).toContainText('Sender left the room', { timeout: 20000 });
  // Back after that: the card can be downloaded again.
  await resume(ana, 600000);
  await expect(bo.page.locator('.file-card .file-download-btn')).toBeVisible({ timeout: 15000 });

  for (const m of [ana, bo]) await m.context.close();
});

test('DMs to an away member wait in RAM and go out when they are back; dropped once they count as left', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await freeze(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'away', { timeout: 20000 });

  await memberRow(ana.page, 'Bo').locator('.dm-btn').click();
  await ana.page.locator('#dm-input').fill('psst, see this when you are back');
  await ana.page.locator('#dm-send-btn').click();
  const line = ana.page.locator('.dm-line.self', { hasText: 'psst' });
  await expect(line).toHaveAttribute('data-delivery', 'waiting');
  await expect(line.locator('.dm-delivery')).toHaveText('Waiting: Bo is away');

  await resume(bo);
  await expect(line).not.toHaveAttribute('data-delivery', /.*/, { timeout: 15000 });
  await memberRow(bo.page, 'Ana').locator('.dm-btn').click();
  await expect(bo.page.locator('.dm-line.peer')).toContainText('psst, see this when you are back');

  await grace(ana, 3000);
  await freeze(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await ana.page.locator('#dm-input').fill('are you there?');
  await ana.page.locator('#dm-send-btn').click();
  const lost = ana.page.locator('.dm-line.self', { hasText: 'are you there?' });
  await expect(lost).toHaveAttribute('data-delivery', 'not-delivered', { timeout: 15000 });
  await expect(lost.locator('.dm-delivery')).toHaveText('Not delivered: Bo left');

  for (const m of [ana, bo]) await m.context.close();
});

test('an away admin keeps the role; the heir takes over only once the admin counts as left', async ({ browser }) => {
  const [ana, bo, cy] = await room(browser, ['Ana', 'Bo', 'Cy']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  // Ana's phone switches apps for longer than the old 15 s: nobody takes over.
  await freeze(ana);
  await expect(memberRow(bo.page, 'Ana')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await bo.page.waitForTimeout(20000);
  await expect(notice(bo.page, 'is now an admin')).toHaveCount(0);
  expect(await keyState(bo.page)).toBe('dormant');
  await resume(ana);
  await expectDirectMesh(bo.page, ['Ana', 'Bo', 'Cy'], 'Bo');

  // Bo, the heir, switches apps: the key goes to nobody else.
  await freeze(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await ana.page.waitForTimeout(5000);
  expect(await keyState(cy.page)).toBe('none');
  await resume(bo);
  await expectDirectMesh(ana.page, ['Ana', 'Bo', 'Cy'], 'Ana');
  expect(await keyState(bo.page)).toBe('dormant');

  // Ana stays away past the grace: then Bo takes over.
  for (const m of [bo, cy]) await grace(m, 2000);
  await freeze(ana);
  for (const m of [bo, cy]) await expect(notice(m.page, 'Ana left')).toBeVisible({ timeout: 25000 });
  for (const m of [bo, cy]) await expect(notice(m.page, 'Bo is now an admin')).toBeVisible({ timeout: 25000 });

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a member away during a kick follows the room to its new link with the same identity', async ({ browser }) => {
  const [ana, bo, cy, dee] = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  const cyKey = await selfKey(cy.page);
  const oldRoom = new URL(cy.page.url()).hash.match(/room=([^&]+)/)[1];

  await freeze(cy);
  await freeze(dee);
  await expect(memberRow(ana.page, 'Cy')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await expect(memberRow(ana.page, 'Dee')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  // Kick Dee (away) and, with it, move the room: Bo follows at once.
  ana.page.once('dialog', (dialog) => dialog.accept());
  await memberRow(ana.page, 'Dee').locator('.kick-btn').click();
  await expect(notice(bo.page, 'moved the room')).toBeVisible({ timeout: 15000 });
  await expect.poll(() => new URL(bo.page.url()).hash.match(/room=([^&]+)/)[1], { timeout: 15000 }).not.toBe(oldRoom);
  const newRoom = new URL(bo.page.url()).hash.match(/room=([^&]+)/)[1];

  // Cy comes back on the old link: a member who moved hands over the rekey.
  await resume(cy);
  await expect.poll(() => new URL(cy.page.url()).hash.match(/room=([^&]+)/)[1], { timeout: 20000 }).toBe(newRoom);
  await expectDirectMesh(cy.page, ['Ana', 'Bo', 'Cy'], 'Cy');
  expect(await selfKey(cy.page)).toBe(cyKey);

  // Dee, kicked while away, learns it on return.
  await resume(dee);
  await expect(dee.page.locator('.room-removed')).toBeVisible({ timeout: 20000 });

  for (const m of [ana, bo, cy, dee]) await m.context.close();
});

test('a member in voice who switches apps is back in voice with audio flowing', async ({ browser }) => {
  const [ana, bo] = await room(browser, ['Ana', 'Bo']);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expectAudioFrom(ana.page, 1);

  await freeze(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await resume(bo);
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'direct', { timeout: 15000 });
  await expect(bo.page.locator('#leave-voice-btn')).toBeVisible();
  await expectAudioFrom(ana.page, 1);
  await expectAudioFrom(bo.page, 1);

  for (const m of [ana, bo]) await m.context.close();
});
