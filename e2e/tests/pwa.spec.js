import crypto from 'node:crypto';
import { test, expect } from '@playwright/test';
import { createRoom, expectNoStorage, inviteFrom, newMember, sendMessage } from './helpers.js';

// The installed app (PWA): manifest and icons, "Join with a link", room links handed to an
// open app window (launchQueue), the install offer and the app badge. Installing itself
// can't happen headless, so the browser's PWA APIs are stubbed where needed.

const IPHONE_UA = 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1';

/** A link to `adminUrl`'s room as another dchat site would print it. */
function foreignLink(adminUrl) {
  return `https://mirror.example/dchat/${new URL(inviteFrom(adminUrl)).hash}`;
}

/** A link to some other room (fresh ID and key). */
function otherRoomLink() {
  return `https://dchat.example/#room=${crypto.randomBytes(8).toString('hex')}&key=${crypto.randomBytes(32).toString('base64url')}`;
}

const roomOf = (page) => page.evaluate(() => new URLSearchParams(location.hash.slice(1)).get('room'));

/** Width and height from a PNG's IHDR chunk. */
function pngSize(bytes) {
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

/** Every URL `page` requests or opens a WebSocket to, from now on. */
function recordRequests(page) {
  const urls = [];
  page.on('request', (r) => urls.push(r.url()));
  page.on('websocket', (ws) => urls.push(ws.url()));
  return urls;
}

test('manifest and icons are served, linked and cached for offline start', async ({ page, request }) => {
  const manifest = await (await request.get('/manifest.webmanifest')).json();
  expect(manifest).toMatchObject({
    id: './',
    start_url: './',
    scope: './',
    display: 'standalone',
    launch_handler: { client_mode: ['navigate-new', 'focus-existing'] },
  });
  // A protocol handler's %s template would carry the room key in a request.
  expect(manifest.protocol_handlers).toBeUndefined();
  expect(manifest.icons.some((i) => i.purpose === 'maskable')).toBe(true);
  const icons = [...manifest.icons, { src: 'icons/apple-touch-icon.png', sizes: '180x180', type: 'image/png' }];
  for (const icon of icons) {
    const response = await request.get(`/${icon.src}`);
    expect(response.status(), icon.src).toBe(200);
    if (icon.type === 'image/png') {
      const [w, h] = icon.sizes.split('x').map(Number);
      expect(pngSize(await response.body()), icon.src).toEqual([w, h]);
    }
  }

  await page.goto('/');
  await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('href', 'manifest.webmanifest');
  await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveAttribute('href', 'icons/apple-touch-icon.png');
  await page.evaluate(() => navigator.serviceWorker.ready);
  await expect.poll(() => page.evaluate(async () => {
    const urls = [];
    for (const name of await caches.keys()) {
      for (const req of await (await caches.open(name)).keys()) urls.push(new URL(req.url).pathname);
    }
    return urls;
  })).toEqual(expect.arrayContaining([
    '/manifest.webmanifest', '/icons/icon.svg', '/icons/icon-192.png', '/icons/icon-512.png',
    '/icons/icon-maskable-512.png', '/icons/apple-touch-icon.png',
  ]));
  await expectNoStorage(page);
});

test('join with a link: any site or a bare fragment; the key never leaves the page', async ({ browser }) => {
  const host = await newMember(browser, 'Host');
  const guest = await newMember(browser, 'Guest');
  const adminUrl = await createRoom(host.page, { name: 'Ana' });
  const key = new URLSearchParams(new URL(adminUrl).hash.slice(1)).get('key');
  const requests = recordRequests(guest.page);

  await guest.page.goto('/');
  await expect(guest.page.locator('#join-link-btn')).toBeDisabled();
  // Anything without a room and a valid key is refused in place.
  for (const bad of ['hello', 'https://mirror.example/dchat/', '#room=abc&key=c2hvcnQ']) {
    await guest.page.locator('#join-link-input').fill(bad);
    await guest.page.locator('#join-link-btn').click();
    await expect(guest.page.locator('.join-link-invalid')).toBeVisible();
    expect(await guest.page.evaluate(() => location.hash)).toBe('');
  }
  // Typing again clears the error.
  await guest.page.locator('#join-link-input').fill('x');
  await expect(guest.page.locator('.join-link-invalid')).toHaveCount(0);

  // Another site's link: only its fragment is used, and the room is joined from here.
  await guest.page.locator('#join-link-input').fill(foreignLink(adminUrl));
  await guest.page.locator('#join-link-btn').click();
  await expect(guest.page.locator('#enter-room-btn')).toBeVisible();
  expect(guest.page.url()).toBe(`http://127.0.0.1:3333/${new URL(inviteFrom(adminUrl)).hash}`);
  await guest.page.locator('#name-input').fill('Bea');
  await guest.page.locator('#enter-room-btn').click();
  await expect(guest.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await sendMessage(host.page, 'welcome in');
  await expect(guest.page.locator('.chat-container')).toContainText('welcome in', { timeout: 5000 });

  // A bare fragment works too.
  const third = await newMember(browser, 'Third');
  await third.page.goto('/');
  await third.page.locator('#join-link-input').fill(`  ${new URL(inviteFrom(adminUrl)).hash}  `);
  await third.page.locator('#join-link-btn').click();
  await expect(third.page.locator('#enter-room-btn')).toBeVisible();
  expect(await roomOf(third.page)).toBe(await roomOf(host.page));

  expect(requests.length).toBeGreaterThan(0);
  for (const url of requests) {
    expect(url).not.toContain('key=');
    expect(url).not.toContain(key);
  }
  await expectNoStorage(guest.page);
  await host.context.close();
  await guest.context.close();
  await third.context.close();
});

test('a launched room link: joins from the lobby, asks before leaving a live room', async ({ browser }) => {
  const host = await newMember(browser, 'Host');
  const app = await newMember(browser, 'App');
  // What an installed app window gets: links delivered through window.launchQueue.
  await app.context.addInitScript(() => {
    Object.defineProperty(window, 'launchQueue', {
      configurable: true,
      value: { setConsumer(consumer) { window.__launch = (url) => consumer({ targetURL: url }); } },
    });
  });
  const launch = (url) => app.page.evaluate((u) => { setTimeout(() => window.__launch(u), 0); }, url);
  const dialogs = [];
  app.page.on('dialog', (dialog) => {
    dialogs.push(dialog.message());
    if (dialog.message().includes('Leave this room') && dialogs.length > 1) dialog.accept();
    else dialog.dismiss();
  });

  const adminUrl = await createRoom(host.page, { name: 'Ana' });
  await app.page.goto('/');
  await app.page.waitForFunction(() => typeof window.__launch === 'function');
  // Opening the app from its icon (start_url) changes nothing.
  await launch('http://127.0.0.1:3333/');
  await app.page.waitForTimeout(300);
  await expect(app.page.locator('#create-room-btn')).toBeVisible();

  // From the lobby a room link is opened at once.
  await launch(foreignLink(adminUrl));
  await expect(app.page.locator('#enter-room-btn')).toBeVisible();
  await app.page.locator('#name-input').fill('Bea');
  await app.page.locator('#enter-room-btn').click();
  await expect(app.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  const room = await roomOf(app.page);

  // The same room again (a new window gets its own link): nothing to ask.
  await app.page.waitForFunction(() => typeof window.__launch === 'function');
  await launch(adminUrl);
  await app.page.waitForTimeout(500);
  expect(dialogs).toEqual([]);

  // Another room while in this one: asked first; dismissing keeps the room.
  const other = otherRoomLink();
  await launch(other);
  await expect.poll(() => dialogs.length).toBe(1);
  expect(dialogs[0]).toContain('Leave this room');
  await expect(app.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)');
  expect(await roomOf(app.page)).toBe(room);

  // Accepting leaves it for the new link.
  await launch(other);
  await expect(app.page.locator('#enter-room-btn')).toBeVisible();
  expect(dialogs.length).toBe(2);
  expect(await roomOf(app.page)).toBe(new URLSearchParams(new URL(other).hash.slice(1)).get('room'));
  await expect(host.page.locator('.member-row')).toHaveCount(1, { timeout: 15000 });
  await expectNoStorage(app.page);
  await host.context.close();
  await app.context.close();
});

/** Fire what Chromium fires when the app can be installed; returns how to read prompt() calls. */
async function offerInstall(page) {
  await page.evaluate(() => {
    const event = new Event('beforeinstallprompt', { cancelable: true });
    event.prompt = () => {
      window.__prompted = (window.__prompted || 0) + 1;
      return Promise.resolve({ outcome: 'accepted' });
    };
    window.dispatchEvent(event);
  });
}

test('install offer: a lobby button only when the browser offers it and the app is not installed', async ({ browser }) => {
  const tab = await newMember(browser, 'Tab');
  await tab.page.goto('/');
  await tab.page.locator('#create-room-btn').waitFor();
  await expect(tab.page.locator('#install-app-btn')).toHaveCount(0);
  await expect(tab.page.locator('.install-hint')).toHaveCount(0);

  await offerInstall(tab.page);
  await expect(tab.page.locator('#install-app-btn')).toBeVisible();
  await tab.page.locator('#install-app-btn').click();
  expect(await tab.page.evaluate(() => window.__prompted)).toBe(1);
  // An offer works once.
  await expect(tab.page.locator('#install-app-btn')).toHaveCount(0);

  // Never inside a room.
  await offerInstall(tab.page);
  await expect(tab.page.locator('#install-app-btn')).toBeVisible();
  await tab.page.locator('#create-room-btn').click();
  await expect(tab.page.locator('.status-indicator')).toBeVisible();
  await expect(tab.page.locator('#install-app-btn')).toHaveCount(0);
  await expectNoStorage(tab.page);
  await tab.context.close();

  // Already running as the installed app: no button even if offered.
  const installed = await newMember(browser, 'Installed');
  await installed.context.addInitScript(() => {
    const original = window.matchMedia.bind(window);
    window.matchMedia = (query) => (query === '(display-mode: standalone)'
      ? { matches: true, media: query, addEventListener() {}, removeEventListener() {} }
      : original(query));
  });
  await installed.page.goto('/');
  await installed.page.locator('#create-room-btn').waitFor();
  await offerInstall(installed.page);
  await installed.page.waitForTimeout(300);
  await expect(installed.page.locator('#install-app-btn')).toHaveCount(0);
  await installed.context.close();
});

test('install hint on iPhone and iPad Safari, gone once added to the home screen', async ({ browser }) => {
  const safari = await browser.newContext({ userAgent: IPHONE_UA });
  const page = await safari.newPage();
  await page.goto('/');
  await expect(page.locator('.install-hint')).toContainText('Add to Home Screen');
  await expect(page.locator('#install-app-btn')).toHaveCount(0);
  await safari.close();

  const homeScreen = await browser.newContext({ userAgent: IPHONE_UA });
  await homeScreen.addInitScript(() => Object.defineProperty(Navigator.prototype, 'standalone', { get: () => true }));
  const app = await homeScreen.newPage();
  await app.goto('/');
  await app.locator('#create-room-btn').waitFor();
  await expect(app.locator('.install-hint')).toHaveCount(0);
  await homeScreen.close();
});

test('the app badge follows the @mention title badge', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  const bea = await newMember(browser, 'Bea');
  await bea.context.addInitScript(() => {
    window.__badges = [];
    Object.defineProperty(Navigator.prototype, 'setAppBadge', {
      configurable: true,
      value(count) { window.__badges.push(count); return Promise.resolve(); },
    });
    Object.defineProperty(Navigator.prototype, 'clearAppBadge', {
      configurable: true,
      value() { window.__badges.push(0); return Promise.resolve(); },
    });
    window.__hidden = false;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => window.__hidden });
  });
  const adminUrl = await createRoom(ana.page, { name: 'Ana' });
  await bea.page.goto(inviteFrom(adminUrl));
  await bea.page.locator('#name-input').fill('Bea');
  await bea.page.locator('#enter-room-btn').click();
  await expect(bea.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // In the background: the mention shows on the icon as well as in the title.
  await bea.page.evaluate(() => { window.__hidden = true; window.__badges.length = 0; });
  await sendMessage(ana.page, 'hey @Bea');
  await expect.poll(() => bea.page.evaluate(() => window.__badges.at(-1))).toBe(1);
  expect(await bea.page.title()).toMatch(/^\(1\) /);

  // Back in front: both cleared.
  await bea.page.evaluate(() => {
    window.__hidden = false;
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect.poll(() => bea.page.evaluate(() => window.__badges.at(-1))).toBe(0);
  expect(await bea.page.title()).not.toMatch(/^\(/);
  await expectNoStorage(bea.page);
  await ana.context.close();
  await bea.context.close();
});
