import { test, expect } from '@playwright/test';
import {
  createRoom,
  downloadVia,
  expectDirectMesh,
  inviteFrom,
  joinRoom,
  memberRow,
  newMember,
  sendMessage,
  shareFile,
} from './helpers.js';

test.describe.configure({ timeout: 150000 });

const row = (page, text) => page.locator('.message-row', { hasText: text });
const card = (page, name) => page.locator('.file-card', { hasText: name });
const notice = (page, text) => page.locator('.system-notice', { hasText: text });
const selfKey = (page) => page.evaluate(() => window.__dchat.selfPubkey());
const pad = (i) => String(i).padStart(2, '0');
const syncStats = async (page) => JSON.parse(await page.evaluate(() => window.__dchat.syncStats()));

/** Leave cleanly (💥 Wipe Session): the others see it at once. */
async function wipe(member) {
  await member.page.locator('header .btn-danger').click();
  await member.context.close();
}

async function editMessage(page, text, newText) {
  await row(page, text).locator('.edit-btn').click();
  await page.locator('footer.input-bar input').fill(newText);
  await page.locator('footer.input-bar input').press('Enter');
}

async function room(browser, names) {
  const members = [];
  for (const name of names) members.push(await newMember(browser, name));
  const adminUrl = await createRoom(members[0].page, { name: names[0] });
  const invite = inviteFrom(adminUrl);
  for (let i = 1; i < names.length; i++) await joinRoom(members[i].page, invite, names[i]);
  for (let i = 0; i < names.length; i++) await expectDirectMesh(members[i].page, names, names[i]);
  return { members, invite, adminUrl };
}

test('always on: a late joiner gets the conversation as members see it, even from members who left', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  // Nothing to choose: there is no history checkbox and no history badge.
  await ana.page.goto('/');
  await ana.page.locator('#create-room-btn').waitFor();
  await expect(ana.page.locator('#history-checkbox')).toHaveCount(0);
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await expect(ana.page.locator('.history-badge')).toHaveCount(0);
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');

  await sendMessage(ana.page, 'First from Ana');
  await sendMessage(bo.page, 'Second from Bo');
  await sendMessage(ana.page, 'Typo herre');
  await sendMessage(bo.page, 'Delete me later');
  await sendMessage(ana.page, 'hey @Cy look');
  await expect(row(bo.page, 'hey @Cy look')).toBeVisible({ timeout: 10000 });
  await editMessage(ana.page, 'Typo herre', 'Typo fixed');
  await expect(row(bo.page, 'Typo fixed')).toBeVisible({ timeout: 10000 });
  bo.page.once('dialog', (dialog) => dialog.accept());
  await row(bo.page, 'Delete me later').locator('.delete-btn').click();
  await expect(row(ana.page, 'Delete me later')).toHaveCount(0, { timeout: 10000 });
  await row(bo.page, 'First from Ana').locator('.react-btn').click();
  await row(bo.page, 'First from Ana').locator('.reaction-option', { hasText: '👍' }).click();
  await expect(row(ana.page, 'First from Ana').locator('.reaction-chip[data-emoji="👍"]')).toHaveText('👍 1', { timeout: 10000 });

  await shareFile(ana.page, 'ana-notes.txt', 'Notes from Ana');
  await shareFile(bo.page, 'bo-draft.txt', 'Draft from Bo');
  await shareFile(bo.page, 'bo-withdrawn.txt', 'Never mind');
  await expect(card(ana.page, 'bo-withdrawn.txt')).toBeVisible({ timeout: 10000 });
  await card(bo.page, 'bo-withdrawn.txt').locator('.file-withdraw-btn').click();
  await expect(card(ana.page, 'bo-withdrawn.txt')).toContainText('Withdrawn by sender', { timeout: 10000 });
  await expect(card(ana.page, 'bo-draft.txt')).toBeVisible();

  // Closing the context skips the clean leave: the dropped link is detected instead, and Bo
  // counts as away until the grace (shortened here) runs out.
  await ana.page.evaluate(() => window.__dchat.awayGraceMs(2000));
  await bo.context.close();
  await expect(ana.page.locator('.member-row')).toHaveCount(1, { timeout: 30000 });

  await joinRoom(cy.page, invite, 'Cy');
  await expect(row(cy.page, 'First from Ana')).toBeVisible({ timeout: 15000 });
  await expect(row(cy.page, 'Second from Bo').locator('.message-author')).toHaveText('Bo');
  await expect(row(cy.page, 'Typo fixed').locator('.edited-mark')).toHaveText(' (edited)');
  await expect(row(cy.page, 'Typo herre')).toHaveCount(0);
  await expect(row(cy.page, 'Delete me later')).toHaveCount(0);
  await expect(row(cy.page, 'First from Ana').locator('.reaction-chip[data-emoji="👍"]')).toHaveText('👍 1');
  await expect(row(cy.page, 'hey @Cy look')).toHaveClass(/mention/);
  await expect(card(cy.page, 'bo-draft.txt')).toContainText('Sender left the room');
  await expect(card(cy.page, 'bo-withdrawn.txt')).toContainText('Withdrawn by sender');
  await expect(card(cy.page, 'bo-withdrawn.txt').locator('.file-download-btn')).toHaveCount(0);
  expect((await downloadVia(cy.page, card(cy.page, 'ana-notes.txt'))).text).toBe('Notes from Ana');
  await expect(notice(cy.page, 'Earlier messages shared by members are shown above.')).toBeVisible();
  await expect(notice(cy.page, "aren't visible")).toHaveCount(0);

  // In the order they were sent, and what happens next comes after.
  const texts = await cy.page.locator('.message-row').allTextContents();
  const order = ['First from Ana', 'Second from Bo', 'Typo fixed', 'hey @Cy look', 'ana-notes.txt']
    .map((t) => texts.findIndex((x) => x.includes(t)));
  expect(order.every((i) => i >= 0)).toBe(true);
  expect(order).toEqual([...order].sort((a, b) => a - b));
  await sendMessage(ana.page, 'Live after Cy joined');
  await expect(cy.page.locator('.message-row').last()).toContainText('Live after Cy joined', { timeout: 10000 });

  for (const m of [ana, cy]) await m.context.close();
});

test('history passes from member to member (Ana → Bo → Cy → Dee), names kept', async ({ browser }) => {
  const { members: [ana, bo], invite } = await room(browser, ['Ana', 'Bo']);
  await sendMessage(ana.page, 'Ana was here');
  await expect(row(bo.page, 'Ana was here')).toBeVisible({ timeout: 10000 });
  await wipe(ana);
  await expect(bo.page.locator('.member-row')).toHaveCount(1, { timeout: 15000 });

  const cy = await newMember(browser, 'Cy');
  await joinRoom(cy.page, invite, 'Cy');
  await expect(row(cy.page, 'Ana was here').locator('.message-author')).toHaveText('Ana', { timeout: 15000 });
  await sendMessage(bo.page, 'Bo was here');
  await expect(row(cy.page, 'Bo was here')).toBeVisible({ timeout: 10000 });
  await wipe(bo);
  await expect(cy.page.locator('.member-row')).toHaveCount(1, { timeout: 15000 });

  const dee = await newMember(browser, 'Dee');
  await joinRoom(dee.page, invite, 'Dee');
  await expect(row(dee.page, 'Ana was here').locator('.message-author')).toHaveText('Ana', { timeout: 15000 });
  await expect(row(dee.page, 'Bo was here').locator('.message-author')).toHaveText('Bo');
  await sendMessage(cy.page, 'Cy is here');
  await expect(row(dee.page, 'Cy is here')).toBeVisible({ timeout: 10000 });

  for (const m of [cy, dee]) await m.context.close();
});

test('a member cut off for a while catches up once its links come back, without duplicates', async ({ browser }) => {
  const { members: [ana, bo, cy] } = await room(browser, ['Ana', 'Bo', 'Cy']);
  await sendMessage(ana.page, 'Before the gap');
  await expect(row(cy.page, 'Before the gap')).toBeVisible({ timeout: 10000 });
  const [anaKey, boKey, cyKey] = await Promise.all([ana, bo, cy].map((m) => selfKey(m.page)));

  // Cut Cy off on both ends of each link: within the grace, each side lists the other as away.
  await ana.page.evaluate((k) => window.__dchat.blockPeer(k), cyKey);
  await bo.page.evaluate((k) => window.__dchat.blockPeer(k), cyKey);
  await cy.page.evaluate((keys) => keys.forEach((k) => window.__dchat.blockPeer(k)), [anaKey, boKey]);
  await expect(memberRow(ana.page, 'Cy')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  for (const name of ['Ana', 'Bo']) await expect(memberRow(cy.page, name)).toHaveAttribute('data-link', 'away', { timeout: 20000 });

  await sendMessage(ana.page, 'Missed one');
  await sendMessage(bo.page, 'Missed two');
  await expect(row(ana.page, 'Missed two')).toBeVisible({ timeout: 10000 });
  await editMessage(ana.page, 'Missed one', 'Missed one, edited');
  await row(bo.page, 'Before the gap').locator('.react-btn').click();
  await row(bo.page, 'Before the gap').locator('.reaction-option', { hasText: '🎉' }).click();
  await expect(row(ana.page, 'Before the gap').locator('.reaction-chip[data-emoji="🎉"]')).toHaveText('🎉 1', { timeout: 10000 });
  await expect(row(cy.page, 'Missed')).toHaveCount(0);

  await ana.page.evaluate((k) => window.__dchat.unblockPeer(k), cyKey);
  await bo.page.evaluate((k) => window.__dchat.unblockPeer(k), cyKey);
  await cy.page.evaluate((keys) => keys.forEach((k) => window.__dchat.unblockPeer(k)), [anaKey, boKey]);
  await expectDirectMesh(cy.page, ['Ana', 'Bo', 'Cy'], 'Cy');

  await expect(row(cy.page, 'Missed one, edited')).toHaveCount(1, { timeout: 15000 });
  await expect(row(cy.page, 'Missed two')).toHaveCount(1);
  await expect(row(cy.page, 'Before the gap').locator('.reaction-chip[data-emoji="🎉"]')).toHaveText('🎉 1');
  // Every member pulled from the others; nobody ends up with a second copy.
  await cy.page.waitForTimeout(3000);
  for (const m of [ana, bo, cy]) {
    for (const text of ['Before the gap', 'Missed one, edited', 'Missed two']) await expect(row(m.page, text)).toHaveCount(1);
  }

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('after a kick the new room keeps the history: authors can still edit, files still download', async ({ browser }) => {
  const { members: [ana, bo, cy] } = await room(browser, ['Ana', 'Bo', 'Cy']);
  await sendMessage(bo.page, 'Bo before the kick');
  await sendMessage(cy.page, 'Cy before the kick');
  await shareFile(ana.page, 'plan.txt', 'The plan');
  for (const m of [ana, bo]) {
    await expect(row(m.page, 'Cy before the kick')).toBeVisible({ timeout: 10000 });
    await expect(card(m.page, 'plan.txt')).toBeVisible({ timeout: 10000 });
  }
  const boKey = await selfKey(bo.page);

  ana.page.once('dialog', (dialog) => dialog.accept());
  await memberRow(ana.page, 'Cy').locator('.kick-btn').click();
  await expect(cy.page.locator('.room-removed')).toBeVisible({ timeout: 15000 });
  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo']]) {
    await expect(notice(m.page, 'The admin moved the room to a new link')).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Bo'], n);
  }
  // The same identity in the new room: Bo still wrote Bo's earlier message.
  expect(await selfKey(bo.page)).toBe(boKey);
  await editMessage(bo.page, 'Bo before the kick', 'Bo edited after the kick');
  await expect(row(ana.page, 'Bo edited after the kick').locator('.edited-mark')).toHaveText(' (edited)', { timeout: 10000 });

  const dee = await newMember(browser, 'Dee');
  await joinRoom(dee.page, inviteFrom(ana.page.url()), 'Dee');
  await expect(row(dee.page, 'Cy before the kick').locator('.message-author')).toHaveText('Cy', { timeout: 15000 });
  await expect(row(dee.page, 'Bo edited after the kick').locator('.edited-mark')).toHaveText(' (edited)');
  expect((await downloadVia(dee.page, card(dee.page, 'plan.txt'))).text).toBe('The plan');

  for (const m of [ana, bo, cy, dee]) await m.context.close();
});

test('a newcomer pulls the history once, however many members hold it', async ({ browser }) => {
  const { members, invite } = await room(browser, ['Ana', 'Bo', 'Cy']);
  for (let i = 1; i <= 20; i++) await sendMessage(members[i % 3].page, `Message ${pad(i)}`);
  for (const m of members) await expect(row(m.page, 'Message 20')).toBeVisible({ timeout: 10000 });

  const dee = await newMember(browser, 'Dee');
  await joinRoom(dee.page, invite, 'Dee');
  await expectDirectMesh(dee.page, ['Ana', 'Bo', 'Cy', 'Dee'], 'Dee');
  for (let i = 1; i <= 20; i++) await expect(row(dee.page, `Message ${pad(i)}`)).toHaveCount(1, { timeout: 15000 });
  // Dee pulled from each of the three in turn; only the first had anything new.
  await expect.poll(async () => (await syncStats(dee.page)).rounds, { timeout: 30000 }).toBeGreaterThanOrEqual(3);
  const stats = await syncStats(dee.page);
  expect(stats.pulled).toBeGreaterThanOrEqual(20);
  expect(stats.pulled).toBeLessThanOrEqual(23);

  for (const m of [...members, dee]) await m.context.close();
});

test('a slow history download shows a loading line while live chat keeps flowing', async ({ browser }) => {
  const { members: [ana, bo], invite } = await room(browser, ['Ana', 'Bo']);
  for (let i = 1; i <= 30; i++) await sendMessage(ana.page, `Backlog ${pad(i)}`);
  await expect(row(bo.page, 'Backlog 30')).toBeVisible({ timeout: 10000 });
  await wipe(bo);
  await expect(ana.page.locator('.member-row')).toHaveCount(1, { timeout: 15000 });
  await ana.page.evaluate(() => window.__dchat.throttleSync(300));

  const cy = await newMember(browser, 'Cy');
  await joinRoom(cy.page, invite, 'Cy');
  await expect(cy.page.locator('.history-loading')).toBeVisible({ timeout: 15000 });
  await expect(cy.page.locator('.history-loading')).toHaveText('Loading earlier messages…');
  await sendMessage(cy.page, 'Cy live while loading');
  await expect(row(ana.page, 'Cy live while loading')).toBeVisible({ timeout: 5000 });
  await sendMessage(ana.page, 'Ana live while Cy loads');
  await expect(row(cy.page, 'Ana live while Cy loads')).toBeVisible({ timeout: 5000 });
  await expect(cy.page.locator('.history-loading')).toBeVisible();

  await expect(cy.page.locator('.history-loading')).toHaveCount(0, { timeout: 30000 });
  for (let i = 1; i <= 30; i++) await expect(row(cy.page, `Backlog ${pad(i)}`)).toHaveCount(1);
  await expect(notice(cy.page, 'Earlier messages shared by members are shown above.')).toBeVisible();

  for (const m of [ana, cy]) await m.context.close();
});

test('old &hist=1 links still work, new rooms never write it, and a reload gets the history back', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  // A lobby address left over from an older build.
  await ana.page.goto('/#hist=1');
  await ana.page.locator('#create-room-btn').waitFor();
  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('room='));
  expect(ana.page.url()).not.toContain('hist=');

  const oldStyleInvite = `${inviteFrom(ana.page.url())}&hist=1`;
  await joinRoom(bo.page, oldStyleInvite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await sendMessage(ana.page, 'Before the reload');
  await expect(row(bo.page, 'Before the reload')).toBeVisible({ timeout: 10000 });

  // A reload wipes this tab; entering again brings back what the room still holds.
  await bo.page.reload();
  await expect(bo.page.locator('#enter-room-btn')).toBeVisible();
  await expect(bo.page.locator('.message-bubble')).toHaveCount(0);
  await bo.page.locator('#enter-room-btn').click();
  await expect(row(bo.page, 'Before the reload')).toBeVisible({ timeout: 20000 });
  await expect(row(bo.page, 'Before the reload').locator('.message-author')).toHaveText('Ana');

  for (const m of [ana, bo]) await m.context.close();
});

test("a message from another room can't be replayed into this one", async ({ browser }) => {
  const { members: [ana, bo] } = await room(browser, ['Ana', 'Bo']);
  await sendMessage(ana.page, 'Secret of room X');
  await expect(row(bo.page, 'Secret of room X')).toBeVisible({ timeout: 10000 });
  const stolen = await bo.page.evaluate(() => window.__dchat.exportEnvelope('Secret of room X'));
  expect(stolen).toContain('Secret of room X');

  // Bo, also a member of room Y, replays it to Cy there.
  const { members: [cy, mallory] } = await room(browser, ['Cy', 'Mallory']);
  const warnings = [];
  cy.page.on('console', (msg) => {
    if (msg.type() === 'warning') warnings.push(msg.text());
  });
  const cyKey = await selfKey(cy.page);
  await mallory.page.evaluate(([k, json]) => window.__dchat.injectEnvelope(k, json), [cyKey, stolen]);
  await expect.poll(() => warnings.some((w) => w.includes('invalid signature')), { timeout: 10000 }).toBe(true);
  await expect(row(cy.page, 'Secret of room X')).toHaveCount(0);

  for (const m of [ana, bo, cy, mallory]) await m.context.close();
});

test('alone in the room: messages, files and voice messages wait for whoever joins', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const admin = await createRoom(ana.page, { name: 'Ana' });
  const input = ana.page.locator('footer.input-bar input');
  await expect(input).toHaveAttribute('placeholder', 'Nobody else is here yet: whoever joins will see your messages');
  await expect(ana.page.locator('footer.input-bar .attach-btn')).toBeEnabled();

  await sendMessage(ana.page, 'Hello, whoever comes');
  await expect(row(ana.page, 'Hello, whoever comes')).toBeVisible();
  await shareFile(ana.page, 'agenda.pdf', 'the agenda', 'For later');
  await expect(card(ana.page, 'agenda.pdf')).toContainText('Shared with the room');
  await ana.page.locator('footer.input-bar .record-btn').click();
  await ana.page.waitForTimeout(1000);
  await ana.page.locator('footer.input-bar .record-btn').click();
  await ana.page.locator('.recorder-review .rec-send').click();
  await expect(ana.page.locator('.media-card[data-media-kind="voice"]')).toHaveCount(1);

  const bo = await newMember(browser, 'Bo');
  await joinRoom(bo.page, inviteFrom(admin), 'Bo');
  await expect(row(bo.page, 'Hello, whoever comes')).toBeVisible({ timeout: 20000 });
  await expect(bo.page.locator('.file-caption', { hasText: 'For later' })).toBeVisible();
  const got = await downloadVia(bo.page, card(bo.page, 'agenda.pdf'));
  expect(got.text).toBe('the agenda');
  await expect(bo.page.locator('.media-card[data-media-kind="voice"] .voice-play')).toBeEnabled({ timeout: 15000 });
  await expect(input).toHaveAttribute('placeholder', 'Type an encrypted message...');

  for (const m of [ana, bo]) await m.context.close();
});
