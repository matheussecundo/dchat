import crypto from 'node:crypto';
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
export async function createRoom(page, { name, max, voiceCap, videoCap, hideIp, password } = {}) {
  await page.goto('/');
  await page.locator('#create-room-btn').waitFor();
  if (name !== undefined) await page.locator('#name-input').fill(name);
  if (hideIp) await page.locator('#hide-ip-checkbox').check();
  if (password !== undefined) {
    // Password rooms are opt-in: the box appears once this is ticked.
    await page.locator('#password-checkbox').check();
    await page.locator('#password-input').fill(password);
  }
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

export async function joinRoom(page, url, name, { password } = {}) {
  await page.goto(url);
  await page.locator('#enter-room-btn').waitFor();
  await page.locator('#name-input').fill(name);
  if (password !== undefined) await page.locator('#password-input').fill(password);
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

/** Share a file with the room from the input bar (with an optional caption). */
export async function shareFile(page, name, content, caption) {
  await page.setInputFiles('#file-input-hidden', { name, mimeType: 'application/pdf', buffer: Buffer.from(content) });
  await expect(page.locator('.attachment-chip')).toContainText(name);
  if (caption) await page.locator('footer.input-bar input').fill(caption);
  await page.locator('footer.input-bar .send-btn').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
}

/** Click Download and return the downloaded bytes (in-memory Blob fallback). */
export async function downloadVia(page, card) {
  await page.evaluate(() => { delete window.showSaveFilePicker; });
  const downloadPromise = page.waitForEvent('download');
  await card.locator('.file-download-btn').click();
  const download = await downloadPromise;
  const chunks = [];
  for await (const chunk of await download.createReadStream()) chunks.push(chunk);
  return { name: download.suggestedFilename(), text: Buffer.concat(chunks).toString('utf-8') };
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

/** Wait until the mic track `page` sends on its first open link has the `expected` settings. */
export const waitForSenderAudio = (page, expected) => page.waitForFunction((exp) => {
  const pc = window.__pcs.find((p) => p.connectionState === 'connected');
  const track = pc?.getSenders().find((s) => s.track?.kind === 'audio')?.track;
  if (!track || track.readyState !== 'live') return false;
  const settings = track.getSettings();
  return Object.entries(exp).every(([k, v]) => settings[k] === v);
}, expected, { timeout: 10000 });

/**
 * The video sender of every open link of `page`: its first encoding, the top-level
 * `degradationPreference`, the codec chosen in `encodings[0].codec` and the track's hint and
 * size. Keys the page left to the browser read `null`. The sender may have no track (video
 * stopped): it is found through its transceiver, whose receiver is always a video one.
 */
export async function videoSenderParams(page) {
  return page.evaluate(() => window.__pcs
    .filter((pc) => pc.connectionState === 'connected')
    .map((pc) => {
      const video = pc.getTransceivers()
        .filter((t) => !t.stopped && t.receiver.track?.kind === 'video' && /send/.test(t.direction));
      const transceiver = video.find((t) => t.sender.track) || video[0];
      if (!transceiver) return null;
      const params = transceiver.sender.getParameters();
      const encoding = params.encodings?.[0] || {};
      const track = transceiver.sender.track;
      const settings = track ? track.getSettings() : {};
      return {
        hasTrack: Boolean(track),
        maxBitrate: encoding.maxBitrate ?? null,
        maxFramerate: encoding.maxFramerate ?? null,
        scaleResolutionDownBy: encoding.scaleResolutionDownBy ?? null,
        codec: encoding.codec?.mimeType ?? null,
        degradationPreference: params.degradationPreference ?? null,
        contentHint: track ? track.contentHint : null,
        width: settings.width ?? null,
        height: settings.height ?? null,
      };
    }));
}

/** `type: kind` RTP stats (`inbound-rtp` / `outbound-rtp`) of video on every open link, with the codec's mimeType. */
export async function videoRtpStats(page, type) {
  return page.evaluate(async (t) => {
    const out = [];
    for (const pc of window.__pcs.filter((p) => p.connectionState === 'connected')) {
      const report = await pc.getStats();
      const entries = [...report.values()];
      const rtp = entries
        .filter((r) => r.type === t && r.kind === 'video')
        .sort((a, b) => (b.bytesReceived ?? b.bytesSent ?? 0) - (a.bytesReceived ?? a.bytesSent ?? 0))[0];
      if (!rtp) {
        out.push(null);
        continue;
      }
      out.push({
        frameWidth: rtp.frameWidth ?? null,
        frameHeight: rtp.frameHeight ?? null,
        framesPerSecond: rtp.framesPerSecond ?? null,
        bytes: rtp.bytesReceived ?? rtp.bytesSent ?? 0,
        codec: report.get(rtp.codecId)?.mimeType ?? null,
        qualityLimitationReason: rtp.qualityLimitationReason ?? null,
      });
    }
    return out;
  }, type);
}

/** The `jitterBufferTarget` of the video receiver on every open link (`null` when unset). */
export async function videoJitterTargets(page) {
  return page.evaluate(() => window.__pcs
    .filter((pc) => pc.connectionState === 'connected')
    .map((pc) => {
      const receiving = pc.getTransceivers()
        .filter((t) => !t.stopped && t.receiver.track?.kind === 'video' && /recv/.test(t.currentDirection || t.direction));
      if (!receiving.length) return undefined;
      return receiving[0].receiver.jitterBufferTarget;
    }));
}

/** Layout of the time code `TIMESTAMP_SCREEN` paints (source pixels of the 1920×1080 canvas). */
const STAMP_LAYOUT = { width: 1920, height: 1080, x0: 80, y0: 80, cell: 80, cols: 16, rows: 6 };

/**
 * Paints the time code (runs in the page, see `TIMESTAMP_SCREEN`). Rows of 16 cells, black
 * for 0 and white for 1, most significant bit first: the paint time's low 32 bits in whole ms
 * (two rows), the same two rows inverted, a 16-bit frame counter and the counter inverted.
 */
function installTimestampScreen(L) {
  navigator.mediaDevices.getDisplayMedia = async () => {
    const canvas = document.createElement('canvas');
    canvas.width = L.width;
    canvas.height = L.height;
    const ctx = canvas.getContext('2d', { alpha: false });
    let counter = 0;
    let lastPaint = -Infinity;
    const paint = () => {
      const stamp = Math.floor(performance.timeOrigin + performance.now()) % 4294967296;
      counter = (counter + 1) & 0xffff;
      ctx.fillStyle = '#404040';
      ctx.fillRect(0, 0, L.width, L.height);
      // A moving bar, so the picture is never entirely still.
      ctx.fillStyle = '#2a6f97';
      ctx.fillRect((counter * 8) % L.width, L.height - 120, 160, 80);
      const rows = [stamp >>> 16, stamp & 0xffff, ~(stamp >>> 16) & 0xffff, ~stamp & 0xffff, counter, ~counter & 0xffff];
      rows.forEach((bits, r) => {
        for (let c = 0; c < L.cols; c += 1) {
          ctx.fillStyle = (bits >> (L.cols - 1 - c)) & 1 ? '#ffffff' : '#000000';
          ctx.fillRect(L.x0 + c * L.cell, L.y0 + r * L.cell, L.cell, L.cell);
        }
      });
      lastPaint = performance.now();
      window.__stampPaints = (window.__stampPaints || 0) + 1;
    };
    // Every animation frame; a 60 Hz timer stands in while animation frames don't run.
    const onFrame = () => {
      paint();
      requestAnimationFrame(onFrame);
    };
    requestAnimationFrame(onFrame);
    setInterval(() => {
      if (performance.now() - lastPaint > 1000 / 30) paint();
    }, 1000 / 60);
    paint();
    const stream = canvas.captureStream(60);
    const track = stream.getVideoTracks()[0];
    const settings = track.getSettings.bind(track);
    track.getSettings = () => ({ ...settings(), displaySurface: 'monitor' });
    return stream;
  };
}

/**
 * Init script: screen sharing returns a real 1920×1080 canvas captured at 60 fps, repainted
 * on every animation frame with its paint time as a time code (`startStampDecoder` reads it).
 * Only `displaySurface: 'monitor'` is added to `getSettings()`: the size is the canvas's own.
 */
export const TIMESTAMP_SCREEN = `(${installTimestampScreen.toString()})(${JSON.stringify(STAMP_LAYOUT)});`;

/**
 * In a viewer's page: decode the time code of every frame `selector` (a tile `<video>`)
 * presents and record its latency (`window.__stamps`): when the frame is shown
 * (`expectedDisplayTime` of `requestVideoFrameCallback`, in this page's
 * `timeOrigin + now` clock) minus when the sharer painted it (the sharer's clock).
 * Misreads (a cell neither black nor white, a check row that doesn't match) and repeated or
 * older frames are rejected. Without `requestVideoFrameCallback` callbacks it falls back to
 * sampling on animation frames (`window.__stamps.mode` says which).
 */
export async function startStampDecoder(page, selector) {
  await page.evaluate(([sel, L]) => {
    const video = document.querySelector(sel);
    const state = { mode: 'rvfc', samples: [], rejected: 0, misread: 0, callbacks: 0, lastStamp: -Infinity, lastCounter: null };
    window.__stamps = state;
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    const wrap = 4294967296;
    const read = (shownAt) => {
      const vw = video.videoWidth;
      const vh = video.videoHeight;
      if (!vw || !vh) return;
      const sx = vw / L.width;
      const sy = vh / L.height;
      const rx = L.x0 * sx;
      const ry = L.y0 * sy;
      const rw = Math.max(1, Math.round(L.cols * L.cell * sx));
      const rh = Math.max(1, Math.round(L.rows * L.cell * sy));
      if (canvas.width !== rw || canvas.height !== rh) {
        canvas.width = rw;
        canvas.height = rh;
      }
      ctx.drawImage(video, rx, ry, L.cols * L.cell * sx, L.rows * L.cell * sy, 0, 0, rw, rh);
      const px = ctx.getImageData(0, 0, rw, rh).data;
      const cellW = rw / L.cols;
      const cellH = rh / L.rows;
      // The middle third of a cell, averaged: 0 (black), 1 (white) or -1 (neither: a misread).
      const bit = (r, c) => {
        const cx = Math.floor((c + 0.5) * cellW);
        const cy = Math.floor((r + 0.5) * cellH);
        const hx = Math.max(0, Math.floor(cellW / 6));
        const hy = Math.max(0, Math.floor(cellH / 6));
        let sum = 0;
        let n = 0;
        for (let y = cy - hy; y <= cy + hy; y += 1) {
          for (let x = cx - hx; x <= cx + hx; x += 1) {
            const i = (y * rw + x) * 4;
            sum += px[i] + px[i + 1] + px[i + 2];
            n += 3;
          }
        }
        const v = sum / n;
        if (v < 80) return 0;
        if (v > 175) return 1;
        return -1;
      };
      const rows = [];
      for (let r = 0; r < L.rows; r += 1) {
        let value = 0;
        for (let c = 0; c < L.cols; c += 1) {
          const b = bit(r, c);
          if (b < 0) {
            state.misread += 1;
            return;
          }
          value = value * 2 + b;
        }
        rows.push(value);
      }
      if ((rows[0] ^ rows[2]) !== 0xffff || (rows[1] ^ rows[3]) !== 0xffff || (rows[4] ^ rows[5]) !== 0xffff) {
        state.misread += 1;
        return;
      }
      const shown = performance.timeOrigin + shownAt;
      const low = rows[0] * 65536 + rows[1];
      // The paint time in full: the 32-bit stamp nearest to (and normally before) `shown`.
      let behind = (((Math.floor(shown) - low) % wrap) + wrap) % wrap;
      if (behind > wrap / 2) behind -= wrap;
      const painted = Math.floor(shown) - behind;
      const counter = rows[4];
      const forward = state.lastCounter === null ? 1 : (counter - state.lastCounter + 0x10000) & 0xffff;
      if (painted <= state.lastStamp || forward === 0 || forward > 0x8000) {
        state.rejected += 1;
        return;
      }
      state.lastStamp = painted;
      state.lastCounter = counter;
      state.samples.push({ at: shown, latency: shown - painted, counter });
    };
    const onFrame = (now, metadata) => {
      state.callbacks += 1;
      read(metadata && typeof metadata.expectedDisplayTime === 'number' ? metadata.expectedDisplayTime : now);
      video.requestVideoFrameCallback(onFrame);
    };
    if (typeof video.requestVideoFrameCallback === 'function') video.requestVideoFrameCallback(onFrame);
    setTimeout(() => {
      if (state.callbacks > 0) return;
      // No presented-frame callbacks: sample whatever frame shows on each animation frame.
      state.mode = 'raf';
      const tick = (now) => {
        read(now);
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }, 2000);
  }, [selector, STAMP_LAYOUT]);
}

/** What `startStampDecoder` recorded so far. */
export async function stampResults(page) {
  return page.evaluate(() => {
    const { mode, samples, rejected, misread, callbacks } = window.__stamps;
    return { mode, samples, rejected, misread, callbacks };
  });
}

/**
 * How far `b`'s `timeOrigin + now` clock is ahead of `a`'s, from the best of `rounds`
 * back-to-back readings (a, b, a). `uncertainty` is half that reading's round trip.
 */
export async function clockOffset(a, b, rounds = 12) {
  const clock = () => performance.timeOrigin + performance.now();
  let best = null;
  for (let i = 0; i < rounds; i += 1) {
    const t0 = await a.evaluate(clock);
    const tb = await b.evaluate(clock);
    const t1 = await a.evaluate(clock);
    const reading = { offset: tb - (t0 + t1) / 2, uncertainty: (t1 - t0) / 2 };
    if (!best || reading.uncertainty < best.uncertainty) best = reading;
  }
  return best;
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

/** Add init scripts to a member's context: each one a script or `[script, arg]`. */
export async function addInitScripts(member, scripts = []) {
  for (const script of scripts) {
    if (Array.isArray(script)) await member.context.addInitScript(script[0], script[1]);
    else await member.context.addInitScript(script);
  }
}

/**
 * Two members in a room together (2-member mesh). `init.ana` / `init.bo` are init scripts
 * for each (see `addInitScripts`), added before either page loads.
 */
export async function twoMembers(browser, createOptions = {}, init = {}) {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  await addInitScripts(ana, init.ana);
  await addInitScripts(bo, init.bo);
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana', ...createOptions }));
  await joinRoom(bo.page, invite, 'Bo');
  await expectDirectMesh(ana.page, ['Ana', 'Bo'], 'Ana');
  await expectDirectMesh(bo.page, ['Ana', 'Bo'], 'Bo');
  return [ana, bo];
}

/** Open an `EncryptedPayload` with the key in `roomUrl`, as anyone holding the link could. */
export function openWithRoomKey(roomUrl, payload) {
  const key = Buffer.from(new URLSearchParams(new URL(roomUrl).hash.slice(1)).get('key'), 'base64url');
  const data = Buffer.from(payload.ciphertext, 'base64url');
  const decipher = crypto.createDecipheriv('chacha20-poly1305', key, Buffer.from(payload.nonce, 'base64url'), {
    authTagLength: 16,
  });
  decipher.setAuthTag(data.subarray(data.length - 16));
  return Buffer.concat([decipher.update(data.subarray(0, data.length - 16)), decipher.final()]).toString();
}

/** Init script: record every text frame the page sends on a data channel (`window.__sentFrames`). */
export const RECORD_SENT_FRAMES = () => {
  window.__sentFrames = [];
  const send = RTCDataChannel.prototype.send;
  RTCDataChannel.prototype.send = function (data) {
    if (typeof data === 'string') window.__sentFrames.push(data);
    return send.call(this, data);
  };
};

/** Init script: screen sharing returns a 1280×720 canvas reporting `displaySurface`. */
export const CANVAS_SCREEN = ({ surface }) => {
  navigator.mediaDevices.getDisplayMedia = async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 1280;
    canvas.height = 720;
    const ctx = canvas.getContext('2d');
    let frame = 0;
    setInterval(() => {
      ctx.fillStyle = `hsl(${(frame += 7) % 360}, 60%, 45%)`;
      ctx.fillRect(0, 0, 1280, 720);
    }, 50);
    const stream = canvas.captureStream(30);
    const track = stream.getVideoTracks()[0];
    const settings = track.getSettings.bind(track);
    track.getSettings = () => ({ ...settings(), displaySurface: surface, width: 1280, height: 720 });
    return stream;
  };
};

/** The recording dchat-host started by playwright.config.js. */
export const AGENT = { port: 7499, code: 'TEST-0000' };

export async function agentEvents() {
  const response = await fetch(`http://127.0.0.1:${AGENT.port}/__test/events`);
  return response.json();
}

/** End any app session and clear what it recorded. */
export async function resetAgent() {
  await fetch(`http://127.0.0.1:${AGENT.port}/__test/reset`, { method: 'POST' });
}

/** In the sharer's page: pair with the test app from the Remote control dialog. */
export async function pairAgent(page, code = AGENT.code) {
  await page.locator('#control-host-btn').click();
  await page.locator('#agent-port-input').fill(String(AGENT.port));
  await page.locator('#agent-code-input').fill(code);
  await page.locator('#agent-connect-btn').click();
}

/** Init script: pointer lock that always succeeds (headless browsers can't really lock). */
export const STUB_POINTER_LOCK = () => {
  let locked = null;
  window.__pointerLockRequests = 0;
  Object.defineProperty(Document.prototype, 'pointerLockElement', { get() { return locked; }, configurable: true });
  Element.prototype.requestPointerLock = function requestPointerLock() {
    window.__pointerLockRequests += 1;
    locked = this;
    setTimeout(() => document.dispatchEvent(new Event('pointerlockchange')), 0);
    return Promise.resolve();
  };
  Document.prototype.exitPointerLock = function exitPointerLock() {
    locked = null;
    setTimeout(() => document.dispatchEvent(new Event('pointerlockchange')), 0);
  };
};

/** Init script: one "standard" gamepad the test drives with `window.__pressPad(i, on)`. */
export const MOCK_GAMEPAD = () => {
  const pad = {
    id: 'Test pad (STANDARD GAMEPAD)',
    index: 0,
    connected: true,
    mapping: 'standard',
    timestamp: 0,
    axes: [0, 0, 0, 0],
    buttons: Array.from({ length: 17 }, () => ({ pressed: false, touched: false, value: 0 })),
  };
  navigator.getGamepads = () => [pad, null, null, null];
  window.__pressPad = (index, on) => {
    pad.buttons[index] = { pressed: on, touched: on, value: on ? 1 : 0 };
    pad.timestamp += 1;
  };
  window.__tiltPad = (axis, value) => {
    pad.axes[axis] = value;
    pad.timestamp += 1;
  };
};
