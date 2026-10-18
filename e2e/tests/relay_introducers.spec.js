import { test, expect } from '@playwright/test';
import {
  createRoom, expectDirectMesh, inviteFrom, joinRoom, memberRow, newMember, recordPublished,
} from './helpers.js';

// Only the room's two introducers (admins first, then the lowest pubkeys) stay on the relays;
// everyone else joins them only while it has to. `relayState()` says who is on them.
test.describe.configure({ timeout: 150000 });

const relayState = (m) => m.page.evaluate(() => window.__dchat.relayState());
const selfKey = (m) => m.page.evaluate(() => window.__dchat.selfPubkey());
const block = (m, keys) => m.page.evaluate((ks) => ks.forEach((k) => window.__dchat.blockPeer(k)), keys);
const unblock = (m, keys) => m.page.evaluate((ks) => ks.forEach((k) => window.__dchat.unblockPeer(k)), keys);
const onRelays = (members) => async () => (await Promise.all(members.map(relayState))).filter((s) => s.pool).length;

/** Members join one at a time; returns them with their names, keys and the invite. */
async function room(browser, names) {
  const members = [];
  for (const name of names) {
    const m = await newMember(browser, name);
    m.name = name;
    members.push(m);
  }
  const invite = inviteFrom(await createRoom(members[0].page, { name: names[0] }));
  for (let i = 1; i < names.length; i++) {
    await joinRoom(members[i].page, invite, names[i]);
    await expectDirectMesh(members[0].page, names.slice(0, i + 1), names[0]);
  }
  for (const m of members) {
    await expectDirectMesh(m.page, names, m.name);
    m.key = await selfKey(m);
  }
  return { members, invite };
}

/** Wait until only the two introducers are on the relays; returns them and the others. */
async function settled(members) {
  await expect.poll(onRelays(members), { timeout: 30000 }).toBe(2);
  const states = await Promise.all(members.map(relayState));
  const introducers = states[0].introducers;
  for (const s of states) expect(s.introducers).toEqual(introducers);
  return {
    introducers: members.filter((m) => introducers.includes(m.key)),
    others: members.filter((m) => !introducers.includes(m.key)),
  };
}

test('only the two introducers stay on the relays; a newcomer meets everyone else over the mesh', async ({ browser }) => {
  const { members, invite } = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  const { introducers, others } = await settled(members);
  // The admin first, then the lowest pubkey; everyone else is on standby.
  expect(introducers[0].name).toBe('Ana');
  for (const m of introducers) expect((await relayState(m)).pool).toBe(true);
  for (const m of others) {
    expect((await relayState(m)).pool).toBe(false);
    await expect(m.page.locator('.relay-badge')).toContainText('(standby)');
  }

  const eve = await newMember(browser, 'Eve');
  const published = await recordPublished(eve, () => invite);
  await joinRoom(eve.page, invite, 'Eve');
  const names = ['Ana', 'Bo', 'Cy', 'Dee', 'Eve'];
  await expectDirectMesh(eve.page, names, 'Eve');
  for (const m of members) await expectDirectMesh(m.page, names, m.name);
  // Through the relays, Eve shook hands with the introducers only.
  const sealedTo = new Set(published.filter((e) => e.signal.payload.signal.kind === 'Sealed').map((e) => e.signal.payload.signal.content.to));
  expect(sealedTo.size).toBeGreaterThan(0);
  for (const to of sealedTo) expect(introducers.map((m) => m.key)).toContain(to);
  eve.name = 'Eve';
  eve.key = await selfKey(eve);
  await settled([...members, eve]);

  for (const m of [...members, eve]) await m.context.close();
});

test('when an introducer leaves, another member takes its place and newcomers still get in', async ({ browser }) => {
  const { members, invite } = await room(browser, ['Ana', 'Bo', 'Cy']);
  const { introducers, others } = await settled(members);
  const [leaving] = introducers.filter((m) => m.name !== 'Ana');
  const [next] = others;

  await leaving.page.goto('about:blank');
  await expect.poll(async () => (await relayState(next)).pool, { timeout: 10000 }).toBe(true);
  expect((await relayState(next)).introducers).toEqual([members[0].key, next.key]);

  const dee = await newMember(browser, 'Dee');
  await joinRoom(dee.page, invite, 'Dee');
  const names = ['Ana', next.name, 'Dee'];
  for (const [m, name] of [[members[0], 'Ana'], [next, next.name], [dee, 'Dee']]) await expectDirectMesh(m.page, names, name);

  for (const m of [...members, dee]) await m.context.close();
});

test('a member cut off from everyone goes back on the relays and leaves them once linked again', async ({ browser }) => {
  const { members } = await room(browser, ['Ana', 'Bo', 'Cy']);
  const { others: [lone] } = await settled(members);
  const rest = members.filter((m) => m !== lone);

  for (const m of rest) await block(m, [lone.key]);
  await block(lone, rest.map((m) => m.key));
  await expect(memberRow(members[0].page, lone.name)).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await expect.poll(async () => (await relayState(lone)).pool, { timeout: 10000 }).toBe(true);

  for (const m of rest) await unblock(m, [lone.key]);
  await unblock(lone, rest.map((m) => m.key));
  await expectDirectMesh(lone.page, ['Ana', 'Bo', 'Cy'], lone.name);
  await expect.poll(async () => (await relayState(lone)).pool, { timeout: 30000 }).toBe(false);

  for (const m of members) await m.context.close();
});

test('a room split in two keeps introducers on both sides and finds itself again', async ({ browser }) => {
  const { members } = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  await settled(members);
  const sideA = [members[0], members[1]];
  const sideB = [members[2], members[3]];
  for (const m of sideA) await block(m, sideB.map((o) => o.key));
  for (const m of sideB) await block(m, sideA.map((o) => o.key));
  await expect(memberRow(members[0].page, 'Cy')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  await expect(memberRow(members[2].page, 'Ana')).toHaveAttribute('data-link', 'away', { timeout: 20000 });
  // Each side is a room of two: both of its members are introducers.
  await expect.poll(onRelays(members), { timeout: 10000 }).toBe(4);

  for (const m of sideA) await unblock(m, sideB.map((o) => o.key));
  for (const m of sideB) await unblock(m, sideA.map((o) => o.key));
  for (const m of members) await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy', 'Dee'], m.name);
  await settled(members);

  for (const m of members) await m.context.close();
});

test('a newcomer who cannot link with the introducers gets in through another member', async ({ browser }) => {
  const { members, invite } = await room(browser, ['Ana', 'Bo', 'Cy']);
  const { introducers, others: [helper] } = await settled(members);

  // Eve refuses links with both introducers, as if her NAT couldn't reach theirs.
  const eve = await newMember(browser, 'Eve');
  await eve.context.addInitScript((keys) => { window.__dchatBlocked = keys; }, introducers.map((m) => m.key));
  await joinRoom(eve.page, invite, 'Eve');
  // An introducer knocks: the other member joins the relays and links with Eve.
  await expect(memberRow(eve.page, helper.name)).toHaveAttribute('data-link', 'direct', { timeout: 90000 });
  for (const m of introducers) {
    await expect(memberRow(eve.page, m.name)).toHaveAttribute('data-link', 'via');
    await expect(memberRow(m.page, 'Eve')).toHaveAttribute('data-link', 'via', { timeout: 15000 });
  }

  for (const m of [...members, eve]) await m.context.close();
});

test('after a rekey with a member away, only the introducers keep listening on the old link', async ({ browser }) => {
  const { members } = await room(browser, ['Ana', 'Bo', 'Cy', 'Dee']);
  await settled(members);
  const away = members[3];
  await away.page.evaluate(() => window.__dchat.simulateFreeze());
  await expect(memberRow(members[0].page, 'Dee')).toHaveAttribute('data-link', 'away', { timeout: 20000 });

  members[0].page.once('dialog', (dialog) => dialog.accept());
  await members[0].page.locator('#rotate-link-btn').click();
  const movers = members.slice(0, 3);
  for (const m of movers) await expect(m.page.locator('.system-notice', { hasText: 'moved the room' })).toBeVisible({ timeout: 15000 });
  for (const m of movers) await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], m.name);
  // Of the three who moved, the two introducers hold the old room's relays for Dee.
  await expect.poll(async () => (await Promise.all(movers.map(relayState))).map((s) => s.forwarding).reduce((a, b) => a + b), { timeout: 30000 }).toBe(2);
  for (const m of movers) {
    const s = await relayState(m);
    expect(s.forwarding).toBe(s.introducer ? 1 : 0);
  }

  // Dee comes back on the old link and is handed the new one.
  await away.page.evaluate(() => window.__dchat.simulateResume(60000));
  const roomOf = (m) => new URL(m.page.url()).hash.match(/room=([^&]+)/)[1];
  await expect.poll(() => roomOf(away), { timeout: 20000 }).toBe(roomOf(members[1]));
  await expectDirectMesh(away.page, ['Ana', 'Bo', 'Cy', 'Dee'], 'Dee');

  for (const m of members) await m.context.close();
});
