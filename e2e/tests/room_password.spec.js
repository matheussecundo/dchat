import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

const PASSWORD = 'correct horse battery';
const hashParam = (url, key) => new URLSearchParams(new URL(url).hash.slice(1)).get(key);

test('a password room needs the link and the password; a wrong password finds nobody', async ({ browser }) => {
  test.setTimeout(90000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  const adminUrl = await createRoom(ana.page, { name: 'Ana', password: PASSWORD });
  // The link carries a salt, never the password.
  const salt = hashParam(adminUrl, 'pw');
  expect(salt).toMatch(/^[A-Za-z0-9_-]{22}$/);
  expect(decodeURIComponent(adminUrl)).not.toContain('horse');
  await expect(ana.page.locator('.password-badge')).toBeVisible({ timeout: 10000 });
  const invite = inviteFrom(adminUrl);

  // The join screen asks for it, and says where to get it.
  await bo.page.goto(invite);
  await expect(bo.page.locator('#password-input')).toBeVisible();
  await expect(bo.page.locator('#password-input')).toHaveAttribute('autocomplete', 'off');
  await expect(bo.page.locator('.password-room-hint')).toHaveText('This room was created with a password. Ask whoever shared the link.');
  await joinRoom(bo.page, invite, 'Bo', { password: 'wrong horse' });
  await joinRoom(cy.page, invite, 'Cy', { password: ` ${PASSWORD} ` });
  await expectDirectMesh(ana.page, ['Ana', 'Cy'], 'Ana');
  await expectDirectMesh(cy.page, ['Ana', 'Cy'], 'Cy');
  await sendMessage(cy.page, 'with the password');
  await expect(ana.page.locator('.message-row', { hasText: 'with the password' })).toBeVisible({ timeout: 10000 });

  // Bo, with the link but the wrong password, sees and is seen by nobody.
  await bo.page.waitForTimeout(3000);
  await expect(bo.page.locator('.member-row')).toHaveCount(1);
  await expect(ana.page.locator('.member-row')).toHaveCount(2);

  // A new link (rekey) keeps the room behind the same password: members follow without retyping it.
  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  for (const [m, n] of [[ana, 'Ana'], [cy, 'Cy']]) {
    await expect(m.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Cy'], n);
  }
  expect(hashParam(ana.page.url(), 'pw')).toBe(salt);
  expect(hashParam(ana.page.url(), 'room')).not.toBe(hashParam(adminUrl, 'room'));
  await sendMessage(ana.page, 'after the new link');
  await expect(cy.page.locator('.message-row', { hasText: 'after the new link' })).toBeVisible({ timeout: 10000 });

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a password is opt-in at creation: unticked means no password, even after one was typed', async ({ browser }) => {
  test.setTimeout(60000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');

  // No password box until the checkbox is ticked.
  await ana.page.goto('/');
  await expect(ana.page.locator('#password-checkbox')).not.toBeChecked();
  await expect(ana.page.locator('#password-input')).toHaveCount(0);

  // Something typed (or filled in by the browser) and then unticked is dropped.
  await ana.page.locator('#password-checkbox').check();
  await expect(ana.page.locator('#password-input')).toHaveAttribute('autocomplete', 'new-password');
  await ana.page.locator('#password-input').fill('saved earlier');
  await ana.page.locator('#password-checkbox').uncheck();
  await expect(ana.page.locator('#password-input')).toHaveCount(0);
  await ana.page.locator('#password-checkbox').check();
  await expect(ana.page.locator('#password-input')).toHaveValue('');
  await ana.page.locator('#password-input').fill('typed again');
  await ana.page.locator('#password-checkbox').uncheck();

  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('room=') && location.hash.includes('key='));
  expect(hashParam(ana.page.url(), 'pw')).toBeNull();
  await expect(ana.page.locator('.status-indicator')).toBeVisible();
  await expect(ana.page.locator('.password-badge')).toHaveCount(0);

  // Joiners enter with the link alone.
  const invite = inviteFrom(ana.page.url());
  await bo.page.goto(invite);
  await expect(bo.page.locator('#enter-room-btn')).toBeVisible();
  await expect(bo.page.locator('#password-input')).toHaveCount(0);
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  // Ticked but left empty: still no password.
  await bo.page.goto('/');
  await bo.page.locator('#password-checkbox').check();
  await bo.page.locator('#create-room-btn').click();
  await bo.page.waitForFunction(() => location.hash.includes('room=') && location.hash.includes('key='));
  expect(hashParam(bo.page.url(), 'pw')).toBeNull();

  // Ticked and typed: a password room.
  await ana.page.goto('/');
  await ana.page.locator('#password-checkbox').check();
  await ana.page.locator('#password-input').fill(PASSWORD);
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('pw='));
  expect(hashParam(ana.page.url(), 'pw')).toMatch(/^[A-Za-z0-9_-]{22}$/);
  await expect(ana.page.locator('.password-badge')).toBeVisible({ timeout: 10000 });

  for (const m of [ana, bo]) await m.context.close();
});
