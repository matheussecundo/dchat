import Turn from 'node-turn';
import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, newMember, sendMessage } from './helpers.js';

// "Hide my IP address": one member's connections go through TURN only, in a room that
// doesn't hide everyone's. Offered on the join screen when the link or the host has TURN.
let turn;
const TURN_PORT = 34782;
const TURN_SERVERS = { iceServers: [{ urls: [`turn:127.0.0.1:${TURN_PORT}?transport=udp`], username: 'dchat', credential: 'test-secret' }] };

test.beforeAll(() => {
  turn = new Turn({
    listeningIps: ['127.0.0.1'],
    relayIps: ['127.0.0.1'],
    listeningPort: TURN_PORT,
    authMech: 'long-term',
    credentials: { dchat: 'test-secret' },
    debugLevel: 'OFF',
  });
  turn.start();
});
test.afterAll(() => turn.stop());

/** The ICE policy and gathered candidate types of each connected link of `page`. */
const links = (page) => page.evaluate(async () => {
  const out = [];
  for (const pc of window.__pcs.filter((p) => p.connectionState === 'connected')) {
    const local = new Set();
    (await pc.getStats()).forEach((r) => { if (r.type === 'local-candidate') local.add(r.candidateType); });
    out.push({ policy: pc.getConfiguration().iceTransportPolicy, local: [...local] });
  }
  return out;
});

async function openJoinScreen(page, invite) {
  await page.goto(invite);
  await page.locator('#enter-room-btn').waitFor();
}

test('only offered with a TURN server; hides one member, the others connect as usual', async ({ browser }) => {
  test.setTimeout(90000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  for (const m of [ana, bo]) await m.context.route('**/ice-servers', (route) => route.fulfill({ json: TURN_SERVERS }));
  await cy.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: '' }));

  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  const invite = inviteFrom(adminUrl);
  // No TURN anywhere: nothing to hide behind.
  await openJoinScreen(cy.page, invite);
  await cy.page.waitForTimeout(1000);
  await expect(cy.page.locator('#hide-my-ip-checkbox')).toHaveCount(0);

  await openJoinScreen(bo.page, invite);
  await bo.page.locator('#hide-my-ip-checkbox').check();
  await bo.page.locator('#name-input').fill('Bo');
  await bo.page.locator('#enter-room-btn').click();
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  await expect(bo.page.locator('.hide-my-ip-badge')).toBeVisible();
  await expect(ana.page.locator('.hide-my-ip-badge')).toHaveCount(0);
  await expect.poll(() => links(bo.page), { timeout: 10000 }).toEqual([{ policy: 'relay', local: ['relay'] }]);
  expect((await links(ana.page)).map((l) => l.policy)).toEqual(['all']);
  // What Ana was given of Bo: TURN relay addresses only.
  const fromBo = await ana.page.evaluate(() => window.__remoteCandidates);
  expect(fromBo.length).toBeGreaterThan(0);
  for (const candidate of fromBo) expect(candidate).toContain(' typ relay');
  await sendMessage(bo.page, 'hidden but here');
  await expect(ana.page.locator('.message-row', { hasText: 'hidden but here' })).toBeVisible({ timeout: 10000 });

  // The choice follows the room to a new link, and is gone after a reload.
  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  await expect(bo.page.locator('.system-notice', { hasText: 'moved the room' })).toBeVisible({ timeout: 15000 });
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  await expect(bo.page.locator('.hide-my-ip-badge')).toBeVisible();
  await expect.poll(async () => (await links(bo.page)).every((l) => l.policy === 'relay'), { timeout: 10000 }).toBe(true);
  await bo.page.reload();
  await bo.page.locator('#enter-room-btn').waitFor();
  await expect(bo.page.locator('#hide-my-ip-checkbox')).not.toBeChecked();

  for (const m of [ana, bo, cy]) await m.context.close();
});
