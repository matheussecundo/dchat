import crypto from 'node:crypto';
import Turn from 'node-turn';
import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

const GOOGLE_STUN = 'stun:stun.l.google.com:19302';

/** Every ICE server URL of the page's first link. */
const iceUrls = (page) => page.evaluate(() =>
  window.__pcs[0].getConfiguration().iceServers.flatMap((s) => [].concat(s.urls)));

/** Open an `EncryptedPayload` with the room key, as a relay that has the link could. */
function openWithRoomKey(roomUrl, payload) {
  const key = Buffer.from(new URLSearchParams(new URL(roomUrl).hash.slice(1)).get('key'), 'base64url');
  const data = Buffer.from(payload.ciphertext, 'base64url');
  const decipher = crypto.createDecipheriv('chacha20-poly1305', key, Buffer.from(payload.nonce, 'base64url'), {
    authTagLength: 16,
  });
  decipher.setAuthTag(data.subarray(data.length - 16));
  return Buffer.concat([decipher.update(data.subarray(0, data.length - 16)), decipher.final()]).toString();
}

test('handshakes through the relays are sealed: the room key alone does not reveal SDP or IP addresses', async ({ browser }) => {
  test.setTimeout(60000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  // Record what Ana publishes to the relay.
  const published = [];
  await ana.context.routeWebSocket('**/nostr', (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      published.push(message);
      server.send(message);
    });
  });

  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  await joinRoom(bo.page, inviteFrom(adminUrl), 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  const boKey = await bo.page.evaluate(() => window.__dchat.selfPubkey());

  const signals = published
    .map((m) => JSON.parse(m))
    .filter(([type]) => type === 'EVENT')
    .map(([, event]) => JSON.parse(openWithRoomKey(adminUrl, JSON.parse(event.content))));
  const kinds = signals.map((s) => s.payload.kind);
  expect(kinds).toContain('Presence');
  expect(kinds).toContain('Sealed');
  for (const signal of signals.filter((s) => s.payload.kind === 'Sealed')) {
    expect(signal.payload.content.to).toBe(boKey);
  }
  const readable = JSON.stringify(signals);
  for (const secret of ['v=0', 'candidate', 'a=fingerprint', 'sdp']) {
    expect(readable).not.toContain(secret);
  }

  await ana.context.close();
  await bo.context.close();
});

test("Google's STUN server is only a fallback when nothing else offers STUN", async ({ browser }) => {
  test.setTimeout(60000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  // Ana's host offers STUN (as the Cloudflare Worker does); Bo's is a plain static host.
  await ana.context.route('**/ice-servers', (route) =>
    route.fulfill({ json: { iceServers: [{ urls: ['stun:stun.host.test:3478'] }] } }));
  await bo.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: '' }));

  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  await joinRoom(bo.page, inviteFrom(adminUrl), 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  expect(await iceUrls(ana.page)).toEqual(['stun:stun.host.test:3478']);
  expect(await iceUrls(bo.page)).toEqual([GOOGLE_STUN]);

  // A room that names its own STUN server (&stun=) never uses Google's.
  const ownStun = `${inviteFrom(adminUrl)}&stun=stun:stun.room.test:3478`;
  const cy = await newMember(browser, 'Cy');
  const di = await newMember(browser, 'Di');
  await cy.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: '' }));
  await di.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: '' }));
  const room = ownStun.replace(/room=[^&]+/, 'room=ownstun1');
  await joinRoom(cy.page, room, 'Cy');
  await joinRoom(di.page, room, 'Di');
  await expectDirectMesh(cy.page, ['Cy', 'Di'], 'Cy');
  expect(await iceUrls(cy.page)).toEqual(['stun:stun.room.test:3478']);

  for (const m of [ana, bo, cy, di]) await m.context.close();
});

test.describe('hiding IP addresses', () => {
  let turn;
  const TURN_PORT = 34780;
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

  test('members connect only through TURN and only ever see its address', async ({ browser }) => {
    test.setTimeout(60000);
    const ana = await newMember(browser, 'Ana');
    const bo = await newMember(browser, 'Bo');
    for (const m of [ana, bo]) {
      await m.context.route('**/ice-servers', (route) => route.fulfill({
        json: { iceServers: [{ urls: [`turn:127.0.0.1:${TURN_PORT}?transport=udp`], username: 'dchat', credential: 'test-secret' }] },
      }));
    }

    const adminUrl = await createRoom(ana.page, { name: 'Ana', hideIp: true });
    expect(adminUrl).toContain('hideip=1');
    await joinRoom(bo.page, inviteFrom(adminUrl), 'Bo');
    await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
    await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
    await expect(ana.page.locator('.hide-ip-badge')).toBeVisible();
    await expect(ana.page.locator('#no-turn-banner')).toHaveCount(0);

    for (const m of [ana, bo]) {
      const link = await m.page.evaluate(async () => {
        const pc = window.__pcs.find((p) => p.connectionState === 'connected');
        const types = { local: new Set(), remote: new Set() };
        (await pc.getStats()).forEach((r) => {
          if (r.type === 'local-candidate') types.local.add(r.candidateType);
          if (r.type === 'remote-candidate') types.remote.add(r.candidateType);
        });
        return {
          policy: pc.getConfiguration().iceTransportPolicy,
          servers: pc.getConfiguration().iceServers.flatMap((s) => [].concat(s.urls)),
          local: [...types.local],
          remote: [...types.remote],
          sdpHostCandidates: /typ (host|srflx)/.test(pc.localDescription.sdp + pc.remoteDescription.sdp),
        };
      });
      expect(link.policy).toBe('relay');
      expect(link.servers).not.toContain(GOOGLE_STUN);
      // Neither side gathers or receives anything but TURN relay addresses.
      expect(link.local).toEqual(['relay']);
      expect(link.remote).toEqual(['relay']);
      expect(link.sdpHostCandidates).toBe(false);
    }

    await sendMessage(bo.page, 'through the relay');
    await expect(ana.page.locator('.message-row', { hasText: 'through the relay' })).toBeVisible({ timeout: 10000 });
    await ana.context.close();
    await bo.context.close();
  });

  test('without a TURN server the room says why nobody can connect', async ({ browser }) => {
    test.setTimeout(60000);
    const ana = await newMember(browser, 'Ana');
    await ana.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: '' }));
    await createRoom(ana.page, { name: 'Ana', hideIp: true });
    await expect(ana.page.locator('#no-turn-banner')).toBeVisible({ timeout: 10000 });
    await ana.context.close();
  });
});
