import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

test.describe.configure({ timeout: 120000 });

const withoutHist = (url) => url.replace(/&hist=1/, '');

test('history on: a late joiner sees signed recent messages, even from members who left', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  await ana.page.goto('/');
  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#history-checkbox').check();
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('hist=1'));
  await expect(ana.page.locator('.history-badge')).toBeVisible();
  const invite = inviteFrom(ana.page.url());

  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await sendMessage(ana.page, 'First from Ana');
  await sendMessage(bo.page, 'Second from Bo');
  await expect(ana.page.locator('.message-row', { hasText: 'Second from Bo' })).toBeVisible({ timeout: 10000 });
  // Closing the context skips the clean leave: the dropped link is detected instead.
  await bo.context.close();
  await expect(ana.page.locator('.member-row')).toHaveCount(1, { timeout: 30000 });

  await joinRoom(cy.page, invite, 'Cy');
  await expect(cy.page.locator('.message-row', { hasText: 'First from Ana' })).toBeVisible({ timeout: 15000 });
  const boLine = cy.page.locator('.message-row', { hasText: 'Second from Bo' });
  await expect(boLine).toBeVisible();
  await expect(boLine.locator('.message-author')).toHaveText('Bo');
  await expect(cy.page.locator('.system-notice', { hasText: 'Earlier messages shared by members are shown above.' })).toBeVisible();
  await expect(cy.page.locator('.system-notice', { hasText: "aren't visible" })).toHaveCount(0);

  // History is ordered before what happens next.
  await sendMessage(ana.page, 'Live after Cy joined');
  await expect(cy.page.locator('.message-row').last()).toContainText('Live after Cy joined', { timeout: 10000 });

  for (const m of [ana, cy]) await m.context.close();
});

test('history off by default; a member whose link lacks hist keeps their own messages private', async ({ browser }) => {
  // Default room: nothing is shared with late joiners.
  {
    const ana = await newMember(browser, 'Ana');
    const cy = await newMember(browser, 'Cy');
    const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
    await expect(ana.page.locator('.history-badge')).toHaveCount(0);
    await joinRoom(cy.page, invite, 'Cy');
    await expectDirectMesh(ana.page, ['Ana', 'Cy'], 'Ana');
    await sendMessage(ana.page, 'Only for those present');
    await cy.context.close();
    const late = await newMember(browser, 'Late');
    await joinRoom(late.page, invite, 'Late');
    await expectDirectMesh(late.page, ['Ana', 'Late'], 'Late');
    await expect(late.page.locator('.system-notice', { hasText: "Messages sent before you joined aren't visible." })).toBeVisible();
    await late.page.waitForTimeout(2000);
    await expect(late.page.locator('.message-row', { hasText: 'Only for those present' })).toHaveCount(0);
    for (const m of [ana, late]) await m.context.close();
  }

  // History room, but Bo joined with a link without hist=1: Bo's messages are not shareable.
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  await ana.page.goto('/');
  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#history-checkbox').check();
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('hist=1'));
  const invite = inviteFrom(ana.page.url());
  await joinRoom(bo.page, withoutHist(invite), 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await sendMessage(ana.page, 'Ana shares this');
  await sendMessage(bo.page, 'Bo keeps this private');
  await expect(ana.page.locator('.message-row', { hasText: 'Bo keeps this private' })).toBeVisible({ timeout: 10000 });

  await joinRoom(cy.page, invite, 'Cy');
  await expect(cy.page.locator('.message-row', { hasText: 'Ana shares this' })).toBeVisible({ timeout: 15000 });
  await cy.page.waitForTimeout(2000);
  await expect(cy.page.locator('.message-row', { hasText: 'Bo keeps this private' })).toHaveCount(0);

  for (const m of [ana, bo, cy]) await m.context.close();
});
