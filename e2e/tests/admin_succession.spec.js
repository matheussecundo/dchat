import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, expectNoStorage, inviteFrom, joinRoom, memberRow, newMember } from './helpers.js';

test.describe.configure({ timeout: 150000 });

const notice = (page, text) => page.locator('.system-notice', { hasText: text });
const keyState = (page) => page.evaluate(() => window.__dchat.adminKeyState());
const adminParam = (url) => new URL(url).hash.match(/(?:^#|&)adm=([^&]+)/)?.[1];

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
  return { members, invite, adminUrl };
}

/** Leave cleanly (💥 Wipe Session): the others see it at once. */
async function wipe(member) {
  await member.page.locator('header .btn-danger').click();
  await member.context.close();
}

/** Re-enter after a reload, under the same name. */
async function reenter(member, name) {
  await member.page.reload();
  await member.page.locator('#enter-room-btn').waitFor();
  await member.page.locator('#name-input').fill(name);
  await member.page.locator('#enter-room-btn').click();
}

test('the admin closes the tab: the longest-present member takes over after a while, and can kick', async ({ browser }) => {
  const { members: [ana, bo, cy], adminUrl } = await room(browser, ['Ana', 'Bo', 'Cy']);
  // Bo quietly holds the admin key; nobody is shown who.
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');
  expect(await keyState(cy.page)).toBe('none');
  await expect(memberRow(cy.page, 'Bo').locator('.member-badge')).toHaveCount(0);

  // Closing the context skips the clean leave: the dropped link is detected instead.
  await ana.context.close();
  await expect(bo.page.locator('.member-row')).toHaveCount(2, { timeout: 30000 });
  await bo.page.waitForTimeout(10000);
  await expect(notice(bo.page, 'is now an admin')).toHaveCount(0);
  for (const m of [bo, cy]) await expect(notice(m.page, 'Bo is now an admin')).toBeVisible({ timeout: 20000 });

  expect(bo.page.url()).toContain('admsk=');
  expect(adminParam(bo.page.url())).toBe(adminParam(adminUrl));
  await expect(memberRow(cy.page, 'Bo').locator('.member-badge')).toBeVisible();
  await expect(bo.page.locator('.copy-admin-btn')).toBeVisible();
  await expect(bo.page.locator('#rotate-link-btn')).toBeVisible();
  await expect(cy.page.locator('.copy-admin-btn')).toHaveCount(0);

  bo.page.once('dialog', (dialog) => dialog.accept());
  await memberRow(bo.page, 'Cy').locator('.kick-btn').click();
  await expect(cy.page.locator('.room-removed')).toBeVisible({ timeout: 15000 });
  await expectNoStorage(bo.page);

  for (const m of [bo, cy]) await m.context.close();
});

test('an admin who reloads is back before anyone takes over', async ({ browser }) => {
  const { members: [ana, bo, cy] } = await room(browser, ['Ana', 'Bo', 'Cy']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  await reenter(ana, 'Ana');
  await expectDirectMesh(ana.page, ['Ana', 'Bo', 'Cy'], 'Ana');
  await expect(memberRow(bo.page, 'Ana').locator('.member-badge')).toBeVisible();
  await bo.page.waitForTimeout(20000);
  for (const m of [bo, cy]) {
    await expect(notice(m.page, 'is now an admin')).toHaveCount(0);
    expect(await keyState(m.page)).not.toBe('active');
    expect(m.page.url()).not.toContain('admsk=');
  }
  await expect(memberRow(ana.page, 'Bo').locator('.member-badge')).toHaveCount(0);
  await expect(memberRow(ana.page, 'Cy').locator('.member-badge')).toHaveCount(0);

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('succession goes on: each new admin hands the key to the next longest-present member', async ({ browser }) => {
  const { members: [ana, bo, cy, dee] } = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  await wipe(ana);
  await expect(notice(dee.page, 'Bo is now an admin')).toBeVisible({ timeout: 30000 });
  // Bo inherited Ana's order: Cy is next.
  await expect.poll(() => keyState(cy.page), { timeout: 10000 }).toBe('dormant');
  expect(await keyState(dee.page)).toBe('none');

  await wipe(bo);
  await expect(notice(dee.page, 'Cy is now an admin')).toBeVisible({ timeout: 30000 });
  expect(await keyState(cy.page)).toBe('active');
  await expect.poll(() => keyState(dee.page), { timeout: 10000 }).toBe('dormant');

  for (const m of [cy, dee]) await m.context.close();
});

test('Make admin hands the key over at once, after a confirmation that it cannot be undone', async ({ browser }) => {
  const { members: [ana, bo, cy] } = await room(browser, ['Ana', 'Bo', 'Cy']);
  for (const m of [bo, cy]) await expect(m.page.locator('.make-admin-btn')).toHaveCount(0);
  await expect(memberRow(ana.page, 'Bo').locator('.make-admin-btn')).toBeVisible();
  await expect(memberRow(ana.page, 'Ana').locator('.make-admin-btn')).toHaveCount(0);

  // Dismissed: nothing happens.
  let message = '';
  ana.page.once('dialog', (dialog) => {
    message = dialog.message();
    dialog.dismiss();
  });
  await memberRow(ana.page, 'Cy').locator('.make-admin-btn').click();
  expect(message).toContain("can't be undone");
  await ana.page.waitForTimeout(2000);
  await expect(memberRow(bo.page, 'Cy').locator('.member-badge')).toHaveCount(0);

  ana.page.once('dialog', (dialog) => dialog.accept());
  await memberRow(ana.page, 'Cy').locator('.make-admin-btn').click();
  for (const m of [ana, bo, cy]) {
    await expect(memberRow(m.page, 'Cy').locator('.member-badge')).toBeVisible({ timeout: 10000 });
    await expect(notice(m.page, 'Cy is now an admin')).toBeVisible();
  }
  expect(cy.page.url()).toContain('admsk=');
  await expect(cy.page.locator('.copy-admin-btn')).toBeVisible();
  await expect(cy.page.locator('#rotate-link-btn')).toBeVisible();
  // Admins can't remove (or promote) each other.
  for (const [m, other] of [[ana, 'Cy'], [cy, 'Ana']]) {
    await expect(memberRow(m.page, other).locator('.kick-btn')).toHaveCount(0);
    await expect(memberRow(m.page, other).locator('.make-admin-btn')).toHaveCount(0);
  }
  await expect(memberRow(cy.page, 'Bo').locator('.kick-btn')).toBeVisible();

  // The new admin's link survives a reload.
  await reenter(cy, 'Cy');
  await expect(memberRow(bo.page, 'Cy').locator('.member-badge')).toBeVisible({ timeout: 20000 });
  await expect(cy.page.locator('.copy-admin-btn')).toBeVisible();

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('the heir changes when it leaves, and a reload loses a dormant key', async ({ browser }) => {
  const { members: [ana, bo, cy, dee] } = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  await wipe(bo);
  await expect.poll(() => keyState(cy.page), { timeout: 10000 }).toBe('dormant');
  expect(await keyState(dee.page)).toBe('none');

  // Kept in RAM only: Cy's reload drops it, and Cy comes back as the newest member.
  await reenter(cy, 'Cy');
  await expectDirectMesh(cy.page, ['Ana', 'Cy', 'Dee'], 'Cy');
  await expect.poll(() => keyState(dee.page), { timeout: 10000 }).toBe('dormant');
  expect(await keyState(cy.page)).toBe('none');

  await wipe(ana);
  await expect(notice(cy.page, 'Dee is now an admin')).toBeVisible({ timeout: 30000 });

  for (const m of [cy, dee]) await m.context.close();
});

test('seniority survives a new link: the same heir takes over in the new room', async ({ browser }) => {
  const { members: [ana, bo, cy] } = await room(browser, ['Ana', 'Bo', 'Cy']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo'], [cy, 'Cy']]) {
    await expect(notice(m.page, 'The admin moved the room to a new link')).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], n);
  }
  await expect.poll(() => keyState(bo.page), { timeout: 15000 }).toBe('dormant');
  expect(await keyState(cy.page)).toBe('none');

  await wipe(ana);
  await expect(notice(cy.page, 'Bo is now an admin')).toBeVisible({ timeout: 30000 });

  for (const m of [bo, cy]) await m.context.close();
});

test('a heir left alone does not take over until someone else is there', async ({ browser }) => {
  const { members: [ana, bo], invite } = await room(browser, ['Ana', 'Bo']);
  await expect.poll(() => keyState(bo.page), { timeout: 10000 }).toBe('dormant');

  await wipe(ana);
  await expect(bo.page.locator('.member-row')).toHaveCount(1, { timeout: 15000 });
  await bo.page.waitForTimeout(20000);
  expect(await keyState(bo.page)).toBe('dormant');
  await expect(notice(bo.page, 'is now an admin')).toHaveCount(0);

  const cy = await newMember(browser, 'Cy');
  await joinRoom(cy.page, invite, 'Cy');
  await expectDirectMesh(bo.page, ['Bo', 'Cy'], 'Bo');
  for (const m of [bo, cy]) await expect(notice(m.page, 'Bo is now an admin')).toBeVisible({ timeout: 25000 });

  for (const m of [bo, cy]) await m.context.close();
});
