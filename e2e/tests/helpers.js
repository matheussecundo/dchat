import { expect } from '@playwright/test';

/** A fresh browser context (an isolated member) whose console errors are logged. */
export async function newMember(browser, label) {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.log(`${label} ERROR:`, msg.text());
  });
  return { context, page };
}

/** Create a room from the lobby. Returns the creator's URL, which is the admin link. */
export async function createRoom(page, { name, max } = {}) {
  await page.goto('/');
  await page.locator('#create-room-btn').waitFor();
  if (name !== undefined) await page.locator('#name-input').fill(name);
  if (max !== undefined) await page.locator('#cap-input').fill(String(max));
  await page.locator('#create-room-btn').click();
  await expect(page.locator('.status-indicator')).toBeVisible();
  await page.waitForFunction(() => location.hash.includes('room=') && location.hash.includes('key='));
  return page.url();
}

/** The shareable invite: the admin link minus the admin secret. */
export function inviteFrom(adminUrl) {
  const url = new URL(adminUrl);
  const params = url.hash.slice(1).split('&').filter((p) => !p.startsWith('admsk='));
  url.hash = params.join('&');
  return url.toString();
}

export async function joinRoom(page, url, name) {
  await page.goto(url);
  await page.locator('#enter-room-btn').waitFor();
  await page.locator('#name-input').fill(name);
  await page.locator('#enter-room-btn').click();
  await expect(page.locator('.status-indicator')).toBeVisible();
}

export async function sendMessage(page, text) {
  await page.locator('footer.input-bar input').fill(text);
  await page.locator('footer.input-bar .send-btn').click();
}

export function memberRow(page, name) {
  return page.locator('.member-row', { has: page.locator('.member-name', { hasText: name }) });
}

/** Wait until `page` lists exactly `names` and reaches every other member directly. */
export async function expectDirectMesh(page, names, self) {
  await expect(page.locator('.member-row')).toHaveCount(names.length, { timeout: 20000 });
  for (const name of names) {
    const link = name === self ? 'me' : 'direct';
    await expect(memberRow(page, name)).toHaveAttribute('data-link', link, { timeout: 20000 });
  }
}

export async function expectNoStorage(page) {
  const storage = await page.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length }));
  expect(storage.local).toBe(0);
  expect(storage.session).toBe(0);
}
