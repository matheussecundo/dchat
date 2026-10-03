import { expect } from '@playwright/test';

// Records every RTCPeerConnection the page creates in `window.__pcs`, for stats checks.
const TRACK_PCS = `
  window.__pcs = [];
  const OrigPC = window.RTCPeerConnection;
  window.RTCPeerConnection = function(...args) {
    const pc = new OrigPC(...args);
    window.__pcs.push(pc);
    return pc;
  };
  window.RTCPeerConnection.prototype = OrigPC.prototype;
`;

/** A fresh browser context (an isolated member) whose console errors are logged. */
export async function newMember(browser, label) {
  const context = await browser.newContext({ permissions: ['microphone', 'camera'] });
  await context.addInitScript(TRACK_PCS);
  const page = await context.newPage();
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.log(`${label} ERROR:`, msg.text());
  });
  return { context, page };
}

/** Create a room from the lobby. Returns the creator's URL, which is the admin link. */
export async function createRoom(page, { name, max, voiceCap, videoCap } = {}) {
  await page.goto('/');
  await page.locator('#create-room-btn').waitFor();
  if (name !== undefined) await page.locator('#name-input').fill(name);
  if (max !== undefined) await page.locator('#cap-input').fill(String(max));
  if (voiceCap !== undefined) await page.locator('#voice-cap-input').fill(String(voiceCap));
  if (videoCap !== undefined) await page.locator('#video-cap-input').fill(String(videoCap));
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

export function voiceChip(page, name) {
  return page.locator('.voice-chip', { has: page.locator('.voice-chip-name', { hasText: name }) });
}

export function videoTile(page, name) {
  return page.locator('.tile', { hasText: name });
}

export async function joinVoice(page) {
  await page.locator('#join-voice-btn').click();
  await expect(page.locator('#leave-voice-btn')).toBeVisible({ timeout: 10000 });
}

/** Wait until audio is arriving from `count` members (inbound RTP bytes growing on that many links). */
export async function expectAudioFrom(page, count) {
  await page.waitForFunction(async (n) => {
    const live = window.__pcs.filter((pc) => pc.connectionState === 'connected');
    let receiving = 0;
    for (const pc of live) {
      let bytes = 0;
      (await pc.getStats()).forEach((r) => {
        if (r.type === 'inbound-rtp' && r.kind === 'audio') bytes += r.bytesReceived || 0;
      });
      if (bytes > 0) receiving += 1;
    }
    return receiving >= n;
  }, count, { timeout: 15000, polling: 500 });
}

/** The track this page currently sends of `kind` on every open link (null when not sending). */
export async function sentTracks(page, kind) {
  return page.evaluate((k) => window.__pcs
    .filter((pc) => pc.connectionState === 'connected')
    .map((pc) => {
      const track = pc.getSenders().map((s) => s.track).find((t) => t && t.kind === k);
      return track ? { id: track.id, enabled: track.enabled, readyState: track.readyState } : null;
    }), kind);
}

/** Wait until the tile video for `name` renders frames. */
export async function expectVideoFrames(page, name) {
  const tile = videoTile(page, name);
  await expect(tile).toBeVisible({ timeout: 15000 });
  await expect.poll(() => tile.locator('video').evaluate((v) => v.videoWidth), { timeout: 15000 }).toBeGreaterThan(0);
}

/** Total audio bytes received on all open links. */
export async function inboundAudioBytes(page) {
  return page.evaluate(async () => {
    let total = 0;
    for (const pc of window.__pcs.filter((p) => p.connectionState === 'connected')) {
      (await pc.getStats()).forEach((r) => {
        if (r.type === 'inbound-rtp' && r.kind === 'audio') total += r.bytesReceived || 0;
      });
    }
    return total;
  });
}

/** Two members in a room together (2-member mesh). */
export async function twoMembers(browser, createOptions = {}) {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana', ...createOptions }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  return [ana, bo];
}
