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

test.describe.configure({ timeout: 90000 });

test('3-member mesh: names, member list, fan-out messages, zero storage, reload wipe', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  const invite = inviteFrom(adminUrl);
  expect(invite).not.toContain('admsk=');

  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');

  const everyone = ['Ana', 'Bo', 'Cy'];
  await expectDirectMesh(ana.page, everyone, 'Ana');
  await expectDirectMesh(bo.page, everyone, 'Bo');
  await expectDirectMesh(cy.page, everyone, 'Cy');

  // The creator's admin proof is visible to everyone; the copy-admin button only to the creator.
  await expect(memberRow(bo.page, 'Ana').locator('.member-badge')).toBeVisible();
  await expect(memberRow(bo.page, 'Bo').locator('.member-badge')).toHaveCount(0);
  await expect(ana.page.locator('.copy-admin-btn')).toBeVisible();
  await expect(bo.page.locator('.copy-admin-btn')).toHaveCount(0);

  await expect(ana.page.locator('.system-notice', { hasText: 'Bo joined' })).toBeVisible();
  await expect(ana.page.locator('.system-notice', { hasText: 'Cy joined' })).toBeVisible();
  await expect(cy.page.locator('.system-notice', { hasText: "Messages sent before you joined aren't visible." })).toBeVisible();

  // Every message reaches both other members, attributed to its author.
  const lines = [
    [ana, 'Ana', 'Hello group from Ana'],
    [bo, 'Bo', 'Bo here, hi both'],
    [cy, 'Cy', 'Cy checking in'],
  ];
  for (const [member, name, text] of lines) {
    await sendMessage(member.page, text);
    for (const other of [ana, bo, cy]) {
      const row = other.page.locator('.message-row', { hasText: text });
      await expect(row).toBeVisible({ timeout: 10000 });
      await expect(row.locator('.message-author')).toHaveText(name);
    }
  }

  for (const member of [ana, bo, cy]) {
    await expectNoStorage(member.page);
  }

  // Reloading wipes Cy's memory; the others see Cy leave.
  await cy.page.reload();
  await expect(cy.page.locator('#enter-room-btn')).toBeVisible();
  await expect(cy.page.locator('.message-bubble')).toHaveCount(0);
  await expect(ana.page.locator('.member-row')).toHaveCount(2, { timeout: 15000 });
  await expect(ana.page.locator('.system-notice', { hasText: 'Cy left' })).toBeVisible();

  for (const member of [ana, bo, cy]) await member.context.close();
});

test('member cap: the latest joiner is turned away and an admin takes a seat', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  const dee = await newMember(browser, 'Dee');

  const adminUrl = await createRoom(ana.page, { name: 'Ana', max: 2 });
  expect(adminUrl).toContain('max=2');
  const invite = inviteFrom(adminUrl);

  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');

  // Third member: the room is full for them, not for the two already inside.
  await joinRoom(cy.page, invite, 'Cy');
  await expect(cy.page.locator('.room-full')).toBeVisible({ timeout: 20000 });
  await expect(ana.page.locator('.member-row')).toHaveCount(2);
  await expect(bo.page.locator('.member-row')).toHaveCount(2);

  // A second admin session (opened from the admin link) bumps the latest non-admin.
  await joinRoom(dee.page, adminUrl, 'Dee');
  await expect(bo.page.locator('.room-full')).toBeVisible({ timeout: 20000 });
  await expect(memberRow(ana.page, 'Dee')).toHaveAttribute('data-link', 'direct', { timeout: 20000 });
  await expect(ana.page.locator('.member-row')).toHaveCount(2, { timeout: 15000 });
  await expect(memberRow(dee.page, 'Dee').locator('.member-badge')).toBeVisible();

  for (const member of [ana, bo, cy, dee]) await member.context.close();
});

test('members without a direct link still exchange text through a mutual member', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');
  await expectDirectMesh(ana.page, ['Ana', 'Bo', 'Cy'], 'Ana');
  await expectDirectMesh(cy.page, ['Ana', 'Bo', 'Cy'], 'Cy');

  // Simulate a NAT pair that cannot connect: Ana and Cy refuse a direct link.
  const anaKey = await ana.page.evaluate(() => window.__dchat.selfPubkey());
  const cyKey = await cy.page.evaluate(() => window.__dchat.selfPubkey());
  await ana.page.evaluate((pk) => window.__dchat.blockPeer(pk), cyKey);
  await cy.page.evaluate((pk) => window.__dchat.blockPeer(pk), anaKey);

  await expect(memberRow(ana.page, 'Cy')).toHaveAttribute('data-link', 'via', { timeout: 15000 });
  await expect(memberRow(ana.page, 'Cy').locator('.member-link')).toHaveText('via Bo');
  await expect(memberRow(cy.page, 'Ana')).toHaveAttribute('data-link', 'via', { timeout: 15000 });
  await expect(memberRow(bo.page, 'Ana')).toHaveAttribute('data-link', 'direct');

  await sendMessage(cy.page, 'Relayed hello from Cy');
  await expect(ana.page.locator('.message-row', { hasText: 'Relayed hello from Cy' })).toBeVisible({ timeout: 10000 });
  await sendMessage(ana.page, 'Relayed reply from Ana');
  await expect(cy.page.locator('.message-row', { hasText: 'Relayed reply from Ana' })).toBeVisible({ timeout: 10000 });

  // Each message arrives exactly once, even though gossip may deliver copies.
  await expect(bo.page.locator('.message-row', { hasText: 'Relayed hello from Cy' })).toHaveCount(1);
  await expect(ana.page.locator('.message-row', { hasText: 'Relayed hello from Cy' })).toHaveCount(1);

  for (const member of [ana, bo, cy]) await member.context.close();
});
