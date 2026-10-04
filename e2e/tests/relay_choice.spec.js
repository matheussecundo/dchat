import { test, expect } from '@playwright/test';
import { expectDirectMesh, inviteFrom, joinRoom, newMember } from './helpers.js';

const LOCAL_RELAY = 'ws://127.0.0.1:3333/nostr';
const PUBLIC = ['wss://relay.damus.io', 'wss://nos.lol', 'wss://relay.primal.net'];

/** Keep tests offline: public relay connections are answered by a stub that closes them. */
async function stubPublicRelays(context) {
  await context.routeWebSocket(/^wss:\/\/(relay\.damus\.io|nos\.lol|relay\.primal\.net)/, (ws) => ws.close());
}

async function openCreateForm(page) {
  await page.goto('/');
  await page.locator('#create-room-btn').waitFor();
}

async function relaysInModal(page) {
  await page.locator('.relay-badge').click();
  const relays = await page.locator('.relays-list li span:last-child').allTextContents();
  await page.locator('.relays-modal .modal-title-row button').click();
  return relays;
}

test('public relays are the default; no relay choice is written to the link', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  await openCreateForm(ana.page);
  await expect(ana.page.locator('#relay-mode')).toHaveValue('public');
  await expect(ana.page.locator('#relay-url')).toHaveCount(0);
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('key='));
  expect(ana.page.url()).not.toContain('relays=');
  await ana.context.close();
});

test('choosing my relay puts exactly that relay in the link, and members connect through it', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  await openCreateForm(ana.page);
  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#relay-mode').selectOption('custom');

  // Invalid input blocks creation.
  await ana.page.locator('#relay-url').fill('https://not-a-relay.example');
  await expect(ana.page.locator('.relay-invalid')).toBeVisible();
  await expect(ana.page.locator('#create-room-btn')).toBeDisabled();

  await ana.page.locator('#relay-url').fill(LOCAL_RELAY);
  await expect(ana.page.locator('.relay-invalid')).toHaveCount(0);
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('key='));
  const hash = decodeURIComponent(new URL(ana.page.url()).hash);
  expect(hash).toContain(`relays=${LOCAL_RELAY}`);
  expect(hash).not.toContain('nostr,');

  await joinRoom(bo.page, inviteFrom(ana.page.url()), 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  expect(await relaysInModal(bo.page)).toEqual([LOCAL_RELAY]);

  await ana.context.close();
  await bo.context.close();
});

test('my relay plus public relays as backup', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  await stubPublicRelays(ana.context);
  await stubPublicRelays(bo.context);

  await openCreateForm(ana.page);
  await ana.page.locator('#name-input').fill('Ana');
  await ana.page.locator('#relay-mode').selectOption('both');
  await ana.page.locator('#relay-url').fill(LOCAL_RELAY);
  await ana.page.locator('#create-room-btn').click();
  await ana.page.waitForFunction(() => location.hash.includes('key='));
  expect(decodeURIComponent(new URL(ana.page.url()).hash)).toContain(`relays=${LOCAL_RELAY},nostr`);

  await joinRoom(bo.page, inviteFrom(ana.page.url()), 'Bo');
  // The public relays are unreachable here (stubbed); the room still works through ours.
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  expect(await relaysInModal(bo.page)).toEqual([LOCAL_RELAY, ...PUBLIC]);

  await ana.context.close();
  await bo.context.close();
});
