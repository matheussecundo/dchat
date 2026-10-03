import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, memberRow, newMember, sendMessage } from './helpers.js';

test.describe.configure({ timeout: 120000 });

async function trio(browser) {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');
  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo'], [cy, 'Cy']]) await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], n);
  return [ana, bo, cy];
}

const row = (page, text) => page.locator('.message-row', { hasText: text });

test('typing indicator, reactions, edit and delete propagate to everyone', async ({ browser }) => {
  const [ana, bo, cy] = await trio(browser);

  // Typing shows up for others and clears when the message arrives.
  await bo.page.locator('footer.input-bar input').pressSequentially('Typing slowly', { delay: 30 });
  await expect(ana.page.locator('.typing-indicator')).toHaveText('Bo is typing…', { timeout: 10000 });
  await bo.page.locator('footer.input-bar .send-btn').click();
  await expect(row(ana.page, 'Typing slowly')).toBeVisible();
  await expect(ana.page.locator('.typing-indicator')).toHaveText('');

  // Reactions converge on every member.
  await sendMessage(ana.page, 'React to this');
  await expect(row(bo.page, 'React to this')).toBeVisible({ timeout: 10000 });
  await row(bo.page, 'React to this').locator('.react-btn').click();
  await row(bo.page, 'React to this').locator('.reaction-option', { hasText: '👍' }).click();
  for (const m of [ana, cy]) {
    await expect(row(m.page, 'React to this').locator('.reaction-chip[data-emoji="👍"]')).toHaveText('👍 1', { timeout: 10000 });
  }
  await row(ana.page, 'React to this').locator('.reaction-chip[data-emoji="👍"]').click();
  await expect(row(cy.page, 'React to this').locator('.reaction-chip[data-emoji="👍"]')).toHaveText('👍 2', { timeout: 10000 });
  await expect(row(ana.page, 'React to this').locator('.reaction-chip.mine')).toHaveCount(1);
  await row(bo.page, 'React to this').locator('.reaction-chip[data-emoji="👍"]').click();
  await expect(row(cy.page, 'React to this').locator('.reaction-chip[data-emoji="👍"]')).toHaveText('👍 1', { timeout: 10000 });

  // Only the author can edit; everyone sees the new text marked as edited.
  await expect(row(bo.page, 'React to this').locator('.edit-btn')).toHaveCount(0);
  await row(ana.page, 'React to this').locator('.edit-btn').click();
  await expect(ana.page.locator('.editing-hint')).toBeVisible();
  await ana.page.locator('footer.input-bar input').fill('React to this (fixed typo)');
  await ana.page.locator('footer.input-bar input').press('Enter');
  for (const m of [ana, bo, cy]) {
    const edited = row(m.page, 'React to this (fixed typo)');
    await expect(edited).toBeVisible({ timeout: 10000 });
    await expect(edited.locator('.edited-mark')).toHaveText(' (edited)');
    // Reactions survive the edit.
    await expect(edited.locator('.reaction-chip')).toHaveText('👍 1');
  }

  // Delete removes the message for everyone.
  await sendMessage(ana.page, 'Oops, wrong room');
  await expect(row(cy.page, 'Oops, wrong room')).toBeVisible({ timeout: 10000 });
  ana.page.once('dialog', (dialog) => dialog.accept());
  await row(ana.page, 'Oops, wrong room').locator('.delete-btn').click();
  for (const m of [ana, bo, cy]) await expect(row(m.page, 'Oops, wrong room')).toHaveCount(0, { timeout: 10000 });

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('private DMs reach only their recipient, also when relayed, and @mentions highlight', async ({ browser }) => {
  const [ana, bo, cy] = await trio(browser);

  // Direct DM: unread badge, panel, reply.
  await memberRow(ana.page, 'Bo').locator('.dm-btn').click();
  await expect(ana.page.locator('#dm-panel')).toContainText('Private chat with Bo');
  await ana.page.locator('#dm-input').fill('psst, just between us');
  await ana.page.locator('#dm-send-btn').click();
  await expect(ana.page.locator('#dm-panel .dm-line.self')).toContainText('psst, just between us');
  await expect(memberRow(bo.page, 'Ana').locator('.dm-unread')).toHaveText('1', { timeout: 10000 });
  await expect(memberRow(cy.page, 'Ana').locator('.dm-unread')).toHaveCount(0);
  await memberRow(bo.page, 'Ana').locator('.dm-btn').click();
  await expect(bo.page.locator('#dm-panel .dm-line.peer')).toContainText('psst, just between us');
  await expect(memberRow(bo.page, 'Ana').locator('.dm-unread')).toHaveCount(0);
  await bo.page.locator('#dm-input').fill('got it');
  await bo.page.locator('#dm-input').press('Enter');
  await expect(ana.page.locator('#dm-panel .dm-line.peer')).toContainText('got it', { timeout: 10000 });
  await expect(cy.page.locator('body')).not.toContainText('psst');

  // Relayed DM: Ana and Cy have no direct link; Bo relays it without being able to read it.
  const anaKey = await ana.page.evaluate(() => window.__dchat.selfPubkey());
  const cyKey = await cy.page.evaluate(() => window.__dchat.selfPubkey());
  await ana.page.evaluate((pk) => window.__dchat.blockPeer(pk), cyKey);
  await cy.page.evaluate((pk) => window.__dchat.blockPeer(pk), anaKey);
  await expect(memberRow(ana.page, 'Cy')).toHaveAttribute('data-link', 'via', { timeout: 15000 });
  await ana.page.locator('#dm-panel button[title="Close"]').click();
  await memberRow(ana.page, 'Cy').locator('.dm-btn').click();
  await ana.page.locator('#dm-input').fill('relayed secret for Cy');
  await ana.page.locator('#dm-send-btn').click();
  await expect(memberRow(cy.page, 'Ana').locator('.dm-unread')).toHaveText('1', { timeout: 10000 });
  await memberRow(cy.page, 'Ana').locator('.dm-btn').click();
  await expect(cy.page.locator('#dm-panel .dm-line.peer')).toContainText('relayed secret for Cy');
  await expect(bo.page.locator('body')).not.toContainText('relayed secret');

  // Mentions: highlighted only for the mentioned member.
  await sendMessage(bo.page, 'hey @Ana, look at this');
  await expect(row(ana.page, 'hey @Ana')).toHaveClass(/mention/, { timeout: 10000 });
  await expect(row(cy.page, 'hey @Ana')).not.toHaveClass(/mention/);

  for (const m of [ana, bo, cy]) await m.context.close();
});
