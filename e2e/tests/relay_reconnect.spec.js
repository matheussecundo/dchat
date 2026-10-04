import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember } from './helpers.js';

test('the app reconnects to a relay that drops, and newcomers can still reach it', async ({ browser }) => {
  test.setTimeout(90000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');

  // Proxy Ana's relay connections so the test can cut them.
  const connections = [];
  await ana.context.routeWebSocket('**/nostr', (ws) => {
    ws.connectToServer();
    connections.push(ws);
  });

  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expect(ana.page.locator('.relay-badge')).toContainText('(1 active)');
  expect(connections.length).toBe(1);

  // The relay drops Ana. Her direct link to Bo is unaffected; the app reconnects by itself.
  await connections[0].close({ code: 1001, reason: 'relay restart' });
  await expect.poll(() => connections.length, { timeout: 15000 }).toBe(2);
  await expect(ana.page.locator('.relay-badge')).toContainText('(1 active)', { timeout: 15000 });
  await expect(ana.page.locator('.member-row[data-link="direct"]')).toHaveCount(1);

  // Signaling works again: a newcomer finds Ana through the relay.
  await joinRoom(cy.page, invite, 'Cy');
  await expectDirectMesh(ana.page, ['Ana', 'Bo', 'Cy'], 'Ana');
  await expectDirectMesh(cy.page, ['Ana', 'Bo', 'Cy'], 'Cy');

  for (const m of [ana, bo, cy]) await m.context.close();
});
