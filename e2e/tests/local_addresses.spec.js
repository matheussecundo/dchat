import Turn from 'node-turn';
import { test, expect } from '@playwright/test';
import { addInitScripts, createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

// Once a page may use the microphone or camera, browsers stop masking host candidates, so
// dchat keeps local network addresses (192.168.x, 10.x, fc00::/7…) for members on the same
// network only. `window.__dchatSameNetwork` (e2e builds) forces that decision.
const LOCAL = /(^|[^\d.])(10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+|169\.254\.\d+\.\d+|100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d+\.\d+|f[cd][0-9a-f]{2}:[0-9a-f:]*|fe80:[0-9a-f:]*)/i;

/** Everything a page learned of the other side's addresses: candidates given and remote SDP. */
const seenFromRemote = (page) => page.evaluate(() => ({
  candidates: window.__remoteCandidates,
  sdp: window.__pcs.map((pc) => pc.remoteDescription?.sdp || '').join('\n'),
}));

test.describe('with a TURN server', () => {
  let turn;
  const TURN_PORT = 34781;
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

  test('members on different networks never see each other\'s local addresses', async ({ browser }) => {
    test.setTimeout(90000);
    const ana = await newMember(browser, 'Ana');
    const bo = await newMember(browser, 'Bo');
    for (const m of [ana, bo]) {
      await addInitScripts(m, [() => { window.__dchatSameNetwork = false; }]);
      await m.context.route('**/ice-servers', (route) => route.fulfill({
        json: { iceServers: [{ urls: [`turn:127.0.0.1:${TURN_PORT}?transport=udp`], username: 'dchat', credential: 'test-secret' }] },
      }));
    }
    const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
    await joinRoom(bo.page, invite, 'Bo');
    await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
    await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
    // Voice renegotiates over the link: its descriptions are scrubbed too.
    await ana.page.locator('#join-voice-btn').click();
    await bo.page.locator('#join-voice-btn').click();
    await expect(bo.page.locator('.voice-chip')).toHaveCount(2, { timeout: 15000 });
    await sendMessage(bo.page, 'no addresses');
    await expect(ana.page.locator('.message-row', { hasText: 'no addresses' })).toBeVisible({ timeout: 10000 });

    for (const m of [ana, bo]) {
      const seen = await seenFromRemote(m.page);
      expect(seen.candidates.length).toBeGreaterThan(0);
      for (const candidate of seen.candidates) {
        expect(candidate).not.toMatch(LOCAL);
        const related = candidate.match(/ raddr (\S+)/);
        if (related) expect(['0.0.0.0', '::']).toContain(related[1]);
      }
      for (const line of seen.sdp.split(/\r?\n/).filter((l) => /^(a=candidate|c=|a=rtcp:)/.test(l))) {
        expect(line).not.toMatch(LOCAL);
      }
    }
    for (const m of [ana, bo]) await m.context.close();
  });
});

test('members on the same network still link through their local addresses', async ({ browser }) => {
  test.setTimeout(60000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  // Both tabs share this machine's public address (or have none, offline): both cases release.
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  for (const m of [ana, bo]) {
    await expect.poll(async () => (await seenFromRemote(m.page)).candidates.some((c) => / typ host/.test(c) && LOCAL.test(c)), { timeout: 10000 }).toBe(true);
  }
  for (const m of [ana, bo]) await m.context.close();
});
