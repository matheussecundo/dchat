import { test, expect } from '@playwright/test';
import {
  createRoom, expectAudioFrom, expectDirectMesh, expectVideoFrames, inviteFrom, joinRoom, joinVoice,
  newMember, sendMessage, voiceChip,
} from './helpers.js';

/** Proxy a member's relay sockets so the test can take the relays away from it. */
async function cuttableRelays(member) {
  const relay = { sockets: [], down: false };
  await member.context.routeWebSocket('**/nostr', (ws) => {
    if (relay.down) {
      ws.close({ code: 1011, reason: 'relay down' });
      return;
    }
    ws.connectToServer();
    relay.sockets.push(ws);
  });
  relay.cut = async () => {
    relay.down = true;
    for (const ws of relay.sockets) await ws.close({ code: 1001, reason: 'relay down' });
  };
  return relay;
}

/** Every open link of `page` finished negotiating (no offer left hanging) and carries `kinds`. */
async function expectNegotiated(page, kinds) {
  const links = () => page.evaluate(() => window.__pcs
    .filter((pc) => pc.connectionState === 'connected')
    .map((pc) => ({ state: pc.signalingState, media: pc.remoteDescription?.sdp.match(/^m=\w+/gm) || [] })));
  await expect.poll(links, { timeout: 15000 })
    .toEqual([{ state: 'stable', media: expect.arrayContaining(kinds.map((k) => `m=${k}`)) }]);
}

test('once linked, voice and video negotiate over the direct link without any relay', async ({ browser }) => {
  test.setTimeout(90000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const relays = [await cuttableRelays(ana), await cuttableRelays(bo)];

  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  // Both lose every relay: from here on nothing can go through Nostr.
  for (const relay of relays) await relay.cut();
  for (const m of [ana, bo]) {
    await expect(m.page.locator('.relay-badge')).toContainText('(connecting...)', { timeout: 10000 });
  }

  // Adding audio and then video tracks renegotiates the link; that must travel over the link itself.
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expectNegotiated(ana.page, ['audio']);
  await expectNegotiated(bo.page, ['audio']);
  await expectAudioFrom(ana.page, 1);
  await expectAudioFrom(bo.page, 1);
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectNegotiated(bo.page, ['audio', 'video']);
  await expectVideoFrames(bo.page, 'Ana');

  // Chat is unaffected and the link never dropped.
  await sendMessage(bo.page, 'still here without relays');
  await expect(ana.page.locator('.message-bubble', { hasText: 'still here without relays' })).toBeVisible();
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  expect(relays.every((r) => r.sockets.length === 1)).toBe(true);

  for (const m of [ana, bo]) await m.context.close();
});
