import { test, expect } from '@playwright/test';
import {
  createRoom, expectDirectMesh, inviteFrom, joinRoom, memberRow, newMember, recordPublished,
} from './helpers.js';

/** Three members linked to each other; their relay events are recorded (`published` is Bo's). */
async function threeMembers(browser) {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  let invite = '';
  const room = () => invite;
  [ana.published, bo.published, cy.published] = [await recordPublished(ana, room), await recordPublished(bo, room), await recordPublished(cy, room)];
  const published = bo.published;
  invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');
  for (const [m, name] of [[ana, 'Ana'], [bo, 'Bo'], [cy, 'Cy']]) {
    await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], name);
  }
  return { ana, bo, cy, published };
}

const kinds = (events) => events.map((e) => e.signal.payload.signal.kind);

/** A linked member sent nothing to the relays; an introducer at most its safety beacon. */
async function expectQuiet(member, sent) {
  if ((await member.page.evaluate(() => window.__dchat.relayState())).introducer) {
    expect(sent.filter((k) => k !== 'Presence')).toEqual([]);
    expect(sent.length).toBeLessThanOrEqual(1);
  } else {
    expect(sent).toEqual([]);
  }
}

test('linked members stay quiet on the relays, and every event has the same size', async ({ browser }) => {
  test.setTimeout(90000);
  const { ana, bo, cy, published } = await threeMembers(browser);

  // Joining: Bo announced itself and shook hands through the relays.
  expect(kinds(published)).toContain('Presence');
  expect(kinds(published)).toContain('Sealed');
  // Beacons, handshakes and ICE all look alike to the relays.
  expect(new Set(published.map((e) => e.content.length)).size).toBe(1);
  const sizes = await bo.page.evaluate(() => window.__dchat.relayFrameSizes());
  for (const size of Object.values(sizes)) expect(size).toBeLessThanOrEqual(2048);

  // Once linked, the members who aren't introducers say nothing; an introducer at most its
  // safety beacon (every 45–75 s).
  const members = [ana, bo, cy];
  const settled = members.map((m) => m.published.length);
  await bo.page.waitForTimeout(20000);
  for (const [i, m] of members.entries()) await expectQuiet(m, kinds(m.published.slice(settled[i])));
  await expectDirectMesh(bo.page, ['Ana', 'Bo', 'Cy'], 'Bo');

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a member back from a pause with live links stays off the relays', async ({ browser }) => {
  test.setTimeout(90000);
  const { ana, bo, cy, published } = await threeMembers(browser);
  const settled = published.length;

  // The ticker missed a minute, but nothing died: the link checks are answered. (An
  // introducer's safety beacon may still fall in this window.)
  await bo.page.evaluate(() => window.__dchat.simulateResume(60000));
  await bo.page.waitForTimeout(8000);
  await expectQuiet(bo, kinds(published.slice(settled)));
  await expectDirectMesh(bo.page, ['Ana', 'Bo', 'Cy'], 'Bo');
  await expect(memberRow(ana.page, 'Bo')).toHaveAttribute('data-link', 'direct');

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a member linked to everyone leaves without telling the relays', async ({ browser }) => {
  test.setTimeout(90000);
  const { ana, bo, cy, published } = await threeMembers(browser);
  const settled = published.length;

  await bo.page.goto('about:blank');
  // The others still see Bo leave at once, over the mesh.
  await expect(memberRow(ana.page, 'Bo')).toHaveCount(0, { timeout: 10000 });
  await expect(memberRow(cy.page, 'Bo')).toHaveCount(0, { timeout: 10000 });
  expect(kinds(published.slice(settled))).not.toContain('PeerLeft');

  for (const m of [ana, bo, cy]) await m.context.close();
});
