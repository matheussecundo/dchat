import { test, expect } from '@playwright/test';
import { createRoom, expectDirectMesh, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

const TURN_URL = 'turn:turn.example.test:3478?transport=udp';
const iceUrls = (page) => page.evaluate(() =>
  window.__pcs[0].getConfiguration().iceServers.flatMap((s) => [].concat(s.urls).map((url) => ({ url, username: s.username }))));

test('host-offered TURN servers are used; without them the app falls back to STUN', async ({ browser }) => {
  test.setTimeout(60000);
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');

  // Ana's host answers like the Cloudflare Worker; Bo's like a plain static host.
  const requested = [];
  await ana.context.route('**/ice-servers', (route) => {
    requested.push(route.request().url());
    route.fulfill({ json: { iceServers: [{ urls: [TURN_URL], username: 'u1', credential: 'c1' }] } });
  });
  await bo.context.route('**/ice-servers', (route) => route.fulfill({ status: 404, body: 'TURN not configured' }));

  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');

  expect(await iceUrls(ana.page)).toContainEqual({ url: TURN_URL, username: 'u1' });
  const boUrls = (await iceUrls(bo.page)).map((s) => s.url);
  expect(boUrls.some((u) => u.startsWith('stun:'))).toBe(true);
  expect(boUrls.some((u) => u.startsWith('turn'))).toBe(false);

  // The request carries nothing about the room (the fragment never leaves the browser).
  expect(requested.length).toBe(1);
  expect(new URL(requested[0]).pathname).toBe('/ice-servers');
  expect(new URL(requested[0]).search).toBe('');

  await sendMessage(bo.page, 'still direct');
  await expect(ana.page.locator('.message-row', { hasText: 'still direct' })).toBeVisible({ timeout: 10000 });

  await ana.context.close();
  await bo.context.close();
});
