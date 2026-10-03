import { test, expect } from '@playwright/test';
import {
  createRoom,
  expectAudioFrom,
  expectDirectMesh,
  inviteFrom,
  joinRoom,
  joinVoice,
  memberRow,
  newMember,
  sendMessage,
} from './helpers.js';

test.describe.configure({ timeout: 120000 });

const roomParam = (url) => new URL(url).hash.match(/room=([^&]+)/)[1];

test('admin kicks a member: everyone else moves to a new room and keeps the chat', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  const invite = inviteFrom(adminUrl);
  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');
  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo'], [cy, 'Cy']]) await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], n);

  // Only the admin gets moderation controls, and never against another admin.
  await expect(memberRow(ana.page, 'Cy').locator('.kick-btn')).toBeVisible();
  await expect(memberRow(ana.page, 'Ana').locator('.kick-btn')).toHaveCount(0);
  await expect(bo.page.locator('.kick-btn')).toHaveCount(0);
  await expect(bo.page.locator('#rotate-link-btn')).toHaveCount(0);

  await sendMessage(bo.page, 'Before the move');
  await expect(ana.page.locator('.message-row', { hasText: 'Before the move' })).toBeVisible();

  const oldRoom = roomParam(ana.page.url());
  ana.page.once('dialog', (dialog) => dialog.accept());
  await memberRow(ana.page, 'Cy').locator('.kick-btn').click();

  await expect(cy.page.locator('.room-removed')).toBeVisible({ timeout: 15000 });
  await expect(cy.page.locator('.room-removed')).toContainText('You were removed from the room');

  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo']]) {
    await expect(m.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Bo'], n);
    expect(roomParam(m.page.url())).not.toBe(oldRoom);
    await expect(m.page.locator('.message-row', { hasText: 'Before the move' })).toBeVisible();
  }
  expect(roomParam(ana.page.url())).toBe(roomParam(bo.page.url()));
  // The admin keeps the admin link (and badge) in the new room.
  expect(ana.page.url()).toContain('admsk=');
  await expect(memberRow(bo.page, 'Ana').locator('.member-badge')).toBeVisible();

  await sendMessage(bo.page, 'After the move');
  await expect(ana.page.locator('.message-row', { hasText: 'After the move' })).toBeVisible({ timeout: 10000 });

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('rotating the link moves everyone, keeps voice, and strands the old invite', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const dee = await newMember(browser, 'Dee');
  const oldInvite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, oldInvite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expectAudioFrom(bo.page, 1);

  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();

  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo']]) {
    await expect(m.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Bo'], n);
    // Both were in voice, so both are back in voice in the new room.
    await expect(m.page.locator('#leave-voice-btn')).toBeVisible({ timeout: 15000 });
    await expect(m.page.locator('.voice-chip')).toHaveCount(2, { timeout: 15000 });
  }

  // Someone arriving with the old invite finds an empty room.
  await joinRoom(dee.page, oldInvite, 'Dee');
  await expect(dee.page.locator('.status-indicator')).toContainText('Waiting for Peer', { timeout: 10000 });
  await dee.page.waitForTimeout(3000);
  await expect(dee.page.locator('.member-row')).toHaveCount(1);
  await expect(ana.page.locator('.member-row')).toHaveCount(2);

  for (const m of [ana, bo, dee]) await m.context.close();
});
