import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember } from './helpers.js';

test('members on different protocol versions never link; the older one is asked to reload', async ({ browser }) => {
  test.setTimeout(90000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  // Bo runs a newer protocol version (e2e builds read this before the app starts).
  await bo.context.addInitScript(() => { window.__dchatProtocolVersion = 999; });

  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  const invite = inviteFrom(adminUrl);
  await joinRoom(bo.page, invite, 'Bo');

  // Ana is outdated next to Bo: she is asked to reload. Bo is only told someone is behind.
  await expect(ana.page.locator('#update-banner')).toBeVisible({ timeout: 15000 });
  await expect(bo.page.locator('.toast')).toContainText('older version', { timeout: 15000 });
  await expect(bo.page.locator('#update-banner')).toHaveCount(0);

  // A member on Ana's version links with her as usual; Bo stays apart from both.
  await joinRoom(cy.page, invite, 'Cy');
  await expectDirectMesh(ana.page, ['Ana', 'Cy'], 'Ana');
  await expectDirectMesh(cy.page, ['Ana', 'Cy'], 'Cy');
  await expect(cy.page.locator('#update-banner')).toBeVisible({ timeout: 15000 });
  await expect(bo.page.locator('.member-row')).toHaveCount(1);
  await expect(bo.page.locator('.member-row[data-link="me"]')).toHaveCount(1);

  // Reload keeps the room link and loads whatever build the host serves now.
  await ana.page.locator('#reload-btn').click();
  await expect(ana.page.locator('#enter-room-btn')).toBeVisible({ timeout: 15000 });
  expect(ana.page.url()).toBe(adminUrl);

  for (const m of [ana, bo, cy]) await m.context.close();
});
