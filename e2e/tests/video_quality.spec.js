import { test, expect } from '@playwright/test';
import {
  TIMESTAMP_SCREEN, addInitScripts, clockOffset, createRoom, expectAudioFrom, expectDirectMesh, expectNoStorage,
  expectVideoFrames, inviteFrom, joinRoom, joinVoice, newMember, startStampDecoder, stampResults, twoMembers,
  videoJitterTargets, videoRtpStats, videoSenderParams, voiceChip, waitForSenderAudio,
} from './helpers.js';

test.describe.configure({ timeout: 90000 });

// protocol::video: maxBitrate = clamp(w·h·fps·bpp, 300 kbps, cap).
const screenBitrate = (w, h, fps) => Math.min(Math.max(Math.round(w * h * fps * 0.1), 300000), 20000000);
const cameraBitrate = (w, h, fps) => Math.min(Math.max(Math.round(w * h * fps * 0.08), 300000), 6000000);

/** Ana (sharing a 1920×1080 time-coded canvas), Bo and Cy, all in voice. */
async function threeInVoice(browser) {
  const names = ['Ana', 'Bo', 'Cy'];
  const members = [];
  for (const name of names) members.push(await newMember(browser, name));
  await addInitScripts(members[0], [TIMESTAMP_SCREEN]);
  const invite = inviteFrom(await createRoom(members[0].page, { name: 'Ana' }));
  await joinRoom(members[1].page, invite, 'Bo');
  await joinRoom(members[2].page, invite, 'Cy');
  for (const [i, m] of members.entries()) await expectDirectMesh(m.page, names, names[i]);
  for (const m of members) await joinVoice(m.page);
  return members;
}

async function shareScreen(sharer, viewers) {
  await sharer.page.locator('#screen-btn').click();
  for (const v of viewers) {
    await expect(voiceChip(v.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 15000 });
    await expectVideoFrames(v.page, 'Ana');
  }
}

/** Pick a preset from the ▾ quick menu next to the camera or screen button. */
async function pickQuick(page, source, preset) {
  await page.locator(`#${source}-quality-btn`).click();
  const menu = page.locator(`#${source}-quality-menu`);
  await expect(menu).toBeVisible();
  await menu.locator(`.quality-option[data-preset="${preset}"]`).click();
  await expect(menu).toHaveCount(0);
}

/** Pick presets in the settings modal (`{ camera: 'hd', screen: 'text' }`). */
async function pickInSettings(page, choices) {
  await page.locator('#audio-settings-btn').click();
  for (const [source, preset] of Object.entries(choices)) {
    await page.locator(`#${source}-preset-${preset}`).check();
    await expect(page.locator(`#${source}-preset-${preset}`)).toBeChecked();
  }
  await closeSettings(page);
}

async function closeSettings(page) {
  await page.locator('.modal-content button:has-text("Close")').click();
  await expect(page.locator('#camera-preset-group')).toHaveCount(0);
}

/** The settings modal shows these presets checked. */
async function expectPresetsChecked(page, { camera, screen }) {
  await page.locator('#audio-settings-btn').click();
  await expect(page.locator(`#camera-preset-${camera}`)).toBeChecked();
  await expect(page.locator(`#screen-preset-${screen}`)).toBeChecked();
  await closeSettings(page);
}

/** Wait until every open link's video sender matches `expected` (a subset of `videoSenderParams`). */
async function expectSenders(page, expected, links = 1, timeout = 15000) {
  await expect.poll(async () => {
    const all = await videoSenderParams(page);
    return all.map((p) => p && Object.fromEntries(Object.keys(expected).map((k) => [k, p[k]])));
  }, { timeout }).toEqual(Array.from({ length: links }, () => expected));
}

const selfKey = (page) => page.evaluate(() => window.__dchat.selfPubkey());

test('screen shares default to Balanced on every link: crisp 30 fps with the computed bitrate ceiling', async ({ browser }) => {
  const [ana, bo, cy] = await threeInVoice(browser);
  await shareScreen(ana, [bo, cy]);
  await expectSenders(ana.page, {
    hasTrack: true,
    width: 1920,
    height: 1080,
    maxFramerate: 30,
    maxBitrate: screenBitrate(1920, 1080, 30),
    scaleResolutionDownBy: 1,
    degradationPreference: 'maintain-resolution',
    contentHint: '',
  }, 2);
  expect(screenBitrate(1920, 1080, 30)).toBe(6220800);
  for (const m of [ana, bo, cy]) await m.context.close();
});

test('switching the screen to Smooth from the quick menu updates every link live', async ({ browser }) => {
  const [ana, bo, cy] = await threeInVoice(browser);
  await shareScreen(ana, [bo, cy]);
  await expectSenders(ana.page, { maxFramerate: 30, degradationPreference: 'maintain-resolution' }, 2);
  const trackBefore = (await videoSenderParams(ana.page))[0];

  await pickQuick(ana.page, 'screen', 'smooth');
  await expectSenders(ana.page, {
    hasTrack: true,
    maxFramerate: 60,
    maxBitrate: screenBitrate(1920, 1080, 60),
    degradationPreference: 'maintain-framerate',
    contentHint: 'motion',
    width: trackBefore.width,
    height: trackBefore.height,
  }, 2);
  // Smooth wants a motion codec, and it is one choice for every viewer.
  const codecs = (await videoSenderParams(ana.page)).map((p) => p.codec);
  expect(new Set(codecs).size).toBe(1);
  if (codecs[0] !== null) expect(['video/H265', 'video/H264', 'video/VP8']).toContain(codecs[0]);

  // The menu marks the current choice.
  await ana.page.locator('#screen-quality-btn').click();
  await expect(ana.page.locator('#screen-quality-menu .quality-option[data-preset="smooth"]')).toHaveAttribute('aria-checked', 'true');
  await expect(ana.page.locator('#screen-quality-menu .quality-option[data-preset="balanced"]')).toHaveAttribute('aria-checked', 'false');
  await ana.page.keyboard.press('Escape');
  await expect(ana.page.locator('#screen-quality-menu')).toHaveCount(0);
  // Viewers keep receiving.
  await expect.poll(async () => (await videoRtpStats(bo.page, 'inbound-rtp')).find((s) => s)?.framesPerSecond ?? 0, { timeout: 15000 })
    .toBeGreaterThan(0);
  for (const m of [ana, bo, cy]) await m.context.close();
});

test('Fastest caps a 1080p screen at 720p for the viewer', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await shareScreen(ana, [bo]);
  // Balanced keeps the full 1920 px.
  await expect.poll(async () => (await videoRtpStats(bo.page, 'inbound-rtp'))[0]?.frameWidth, { timeout: 15000 }).toBe(1920);

  await pickQuick(ana.page, 'screen', 'fastest');
  await expectSenders(ana.page, { maxFramerate: 60, degradationPreference: 'maintain-framerate', contentHint: 'motion' });
  // The capture itself scales down, or the sender does (scaleResolutionDownBy): either way
  // at most 1280 px wide are encoded.
  const sender = (await videoSenderParams(ana.page))[0];
  expect(sender.width / sender.scaleResolutionDownBy).toBeLessThanOrEqual(1280);
  if (sender.width > 1280) expect(sender.scaleResolutionDownBy).toBeCloseTo(sender.width / 1280, 3);
  expect(sender.maxBitrate).toBe(screenBitrate(Math.round(sender.width / sender.scaleResolutionDownBy),
    Math.round(sender.height / sender.scaleResolutionDownBy), 60));
  await expect.poll(async () => {
    const width = (await videoRtpStats(bo.page, 'inbound-rtp'))[0]?.frameWidth ?? 0;
    return width > 0 && width <= 1280;
  }, { timeout: 15000 }).toBe(true);
  const sent = (await videoRtpStats(ana.page, 'outbound-rtp'))[0];
  expect(sent.frameWidth).toBeLessThanOrEqual(1280);
  await ana.context.close();
  await bo.context.close();
});

test('a camera after a Fastest screen share starts from browser defaults (no stale settings)', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await pickQuick(ana.page, 'screen', 'fastest');
  await shareScreen(ana, [bo]);
  await expectSenders(ana.page, { maxFramerate: 60, degradationPreference: 'maintain-framerate', contentHint: 'motion' });
  const fastest = (await videoSenderParams(ana.page))[0];
  expect(fastest.maxBitrate).toBeGreaterThan(0);

  // Stop sharing, then the camera on Balanced: every key the screen preset set is gone.
  await ana.page.locator('#screen-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'none', { timeout: 10000 });
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectVideoFrames(bo.page, 'Ana');
  await expectSenders(ana.page, {
    hasTrack: true,
    width: 640,
    height: 480,
    maxBitrate: null,
    maxFramerate: null,
    codec: null,
    degradationPreference: null,
    scaleResolutionDownBy: 1,
    contentHint: '',
  });
  await ana.context.close();
  await bo.context.close();
});

test('camera HD captures 1280×720 with a balanced 30 fps encoder; back to Balanced reverts', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await pickInSettings(ana.page, { camera: 'hd' });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await ana.page.locator('#camera-btn').click();
  await expectVideoFrames(bo.page, 'Ana');
  await expectSenders(ana.page, {
    hasTrack: true,
    width: 1280,
    height: 720,
    maxFramerate: 30,
    maxBitrate: cameraBitrate(1280, 720, 30),
    degradationPreference: 'balanced',
    scaleResolutionDownBy: 1,
  });
  // What Bo receives follows the bandwidth estimate (`balanced` may scale it down), so only
  // the sender's settings are checked.
  await pickQuick(ana.page, 'camera', 'balanced');
  await expectSenders(ana.page, {
    width: 640,
    height: 480,
    maxFramerate: null,
    maxBitrate: null,
    degradationPreference: null,
    scaleResolutionDownBy: 1,
  });
  await ana.context.close();
  await bo.context.close();
});

test('video goes under its own msid, never the voice stream', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await ana.page.locator('#camera-btn').click();
  await expectVideoFrames(bo.page, 'Ana');
  const msids = () => ana.page.evaluate(() => {
    const pc = window.__pcs.find((p) => p.connectionState === 'connected');
    const sdp = pc?.localDescription?.sdp || '';
    const out = { audio: [], video: [] };
    for (const section of sdp.split(/\r\n(?=m=)/).slice(1)) {
      const kind = section.slice(2, 7);
      const streams = [...section.matchAll(/^a=msid:(\S+) \S+$/gm)].map((m) => m[1]).filter((s) => s !== '-');
      if (kind in out) out[kind].push(...streams);
    }
    return out;
  });
  await expect.poll(async () => {
    const { audio, video } = await msids();
    return audio.length > 0 && video.length > 0;
  }, { timeout: 10000 }).toBe(true);
  const { audio, video } = await msids();
  for (const id of video) expect(audio).not.toContain(id);
  await ana.context.close();
  await bo.context.close();
});

test('camera video waits for its voice on the viewer; a screen does not', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await ana.page.locator('#camera-btn').click();
  await expectAudioFrom(bo.page, 1);
  await expectVideoFrames(bo.page, 'Ana');
  // Re-synced every 2 s from the audio jitter buffer.
  await expect.poll(async () => (await videoJitterTargets(bo.page))[0] ?? 0, { timeout: 15000 }).toBeGreaterThan(0);

  // The same sender switches to a screen: the receiver stops waiting.
  await ana.page.locator('#screen-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 10000 });
  await expect.poll(async () => (await videoJitterTargets(bo.page))[0], { timeout: 15000 }).toBeNull();
  await ana.context.close();
  await bo.context.close();
});

test('presets live in RAM: kept across voice rejoins and a rotate-link rekey (with audio settings), reset on reload', async ({ browser }) => {
  test.setTimeout(150000);
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  const hd = { hasTrack: true, width: 1280, height: 720, maxFramerate: 30, degradationPreference: 'balanced' };
  const text = { hasTrack: true, width: 1920, maxFramerate: 15, degradationPreference: 'maintain-resolution', contentHint: 'text' };
  await pickInSettings(ana.page, { camera: 'hd', screen: 'text' });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await ana.page.locator('#camera-btn').click();
  await expectSenders(ana.page, hd);

  // Leave and rejoin voice: still chosen, still applied.
  await ana.page.locator('#leave-voice-btn').click();
  await joinVoice(ana.page);
  await expectPresetsChecked(ana.page, { camera: 'hd', screen: 'text' });
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectSenders(ana.page, hd);
  await ana.page.locator('#screen-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 10000 });
  await expectSenders(ana.page, text);

  // Mic processing changed before the rekey.
  await ana.page.locator('#audio-settings-btn').click();
  await ana.page.locator('#audio-ns').uncheck();
  await waitForSenderAudio(ana.page, { noiseSuppression: false });
  await closeSettings(ana.page);

  // The admin rotates the link: a new session, voice rejoined.
  ana.page.once('dialog', (dialog) => dialog.accept());
  await ana.page.locator('#rotate-link-btn').click();
  for (const [m, n] of [[ana, 'Ana'], [bo, 'Bo']]) {
    await expect(m.page.locator('.system-notice', { hasText: 'The admin moved the room to a new link' })).toBeVisible({ timeout: 15000 });
    await expectDirectMesh(m.page, ['Ana', 'Bo'], n);
    await expect(m.page.locator('#leave-voice-btn')).toBeVisible({ timeout: 15000 });
  }
  // The mic comes back with the same processing (it used to reset), and the presets apply.
  await waitForSenderAudio(ana.page, { noiseSuppression: false, echoCancellation: true, autoGainControl: true });
  await expectPresetsChecked(ana.page, { camera: 'hd', screen: 'text' });
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectSenders(ana.page, hd);
  await ana.page.locator('#screen-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 10000 });
  await expectSenders(ana.page, text);

  // Nothing stored; a reload starts from the defaults.
  await expectNoStorage(ana.page);
  await expectNoStorage(bo.page);
  await ana.page.reload();
  await ana.page.locator('#enter-room-btn').click();
  await expectPresetsChecked(ana.page, { camera: 'balanced', screen: 'balanced' });
  await ana.page.locator('#audio-settings-btn').click();
  await expect(ana.page.locator('#audio-ns')).toBeChecked();
  await closeSettings(ana.page);
  await expectNoStorage(ana.page);
  await ana.context.close();
  await bo.context.close();
});

test('without screen capture (mobile) only the camera quality controls show', async ({ browser }) => {
  const noScreen = () => {
    delete MediaDevices.prototype.getDisplayMedia;
  };
  const [ana, bo] = await twoMembers(browser, {}, { ana: [noScreen] });
  await joinVoice(ana.page);
  await expect(ana.page.locator('#camera-quality-btn')).toBeVisible();
  await expect(ana.page.locator('#screen-quality-btn')).toHaveCount(0);
  await ana.page.locator('#camera-quality-btn').click();
  await expect(ana.page.locator('#camera-quality-menu .quality-option')).toHaveCount(5);
  await ana.page.keyboard.press('Escape');

  await ana.page.locator('#audio-settings-btn').click();
  await expect(ana.page.locator('#camera-preset-group')).toBeVisible();
  await expect(ana.page.locator('#camera-preset-group input[type="radio"]')).toHaveCount(5);
  await expect(ana.page.locator('#screen-preset-group')).toHaveCount(0);
  await closeSettings(ana.page);

  // A browser that can share its screen shows both.
  await joinVoice(bo.page);
  await expect(bo.page.locator('#screen-quality-btn')).toBeVisible();
  await ana.context.close();
  await bo.context.close();
});

test('live stats on demand: numbers for viewer and sharer, never an address', async ({ browser }) => {
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await shareScreen(ana, [bo]);
  const [anaKey, boKey] = [await selfKey(ana.page), await selfKey(bo.page)];
  const tile = (page, pk) => page.locator(`.tile[data-pubkey="${pk}"]`);

  // Bo, watching Ana's screen.
  await tile(bo.page, anaKey).locator('.tile-stats-btn').click();
  const viewer = tile(bo.page, anaKey).locator('.tile-stats[data-side="viewer"]');
  await expect(viewer).toHaveAttribute('data-state', 'ready', { timeout: 10000 });
  const value = (panel, name) => panel.locator(`.stat[data-stat="${name}"] .stat-value`).first();
  await expect(value(viewer, 'fps')).toHaveText(/\d/, { timeout: 10000 });
  await expect(value(viewer, 'resolution')).toHaveText(/\d+×\d+/);
  await expect(value(viewer, 'bitrate')).toHaveText(/\d/);
  await expect(value(viewer, 'codec')).toHaveText(/VP8|VP9|AV1|H264|H265/);

  // Ana, on her own tile: one row per viewer.
  await tile(ana.page, anaKey).locator('.tile-stats-btn').click();
  const sharer = tile(ana.page, anaKey).locator('.tile-stats[data-side="sharer"]');
  await expect(sharer).toHaveAttribute('data-state', 'ready', { timeout: 10000 });
  const row = sharer.locator(`.stats-viewer[data-viewer="${boKey}"]`);
  await expect(row).toHaveCount(1, { timeout: 10000 });
  await expect(value(row, 'fps')).toHaveText(/\d/, { timeout: 10000 });
  await expect(value(row, 'resolution')).toHaveText(/\d+×\d+/);
  await expect(value(row, 'bitrate')).toHaveText(/\d/);

  for (const panel of [viewer, sharer]) {
    const textContent = await panel.textContent();
    expect(textContent).not.toMatch(/\b(?:\d{1,3}\.){3}\d{1,3}\b/);
    expect(textContent).not.toMatch(/(?:[0-9a-f]{1,4}:){2,7}[0-9a-f]{0,4}|::/i);
    expect(textContent).not.toMatch(/candidate|\.local\b/i);
  }

  // Closing stops it.
  await tile(bo.page, anaKey).locator('.tile-stats-btn').click();
  await expect(tile(bo.page, anaKey).locator('.tile-stats')).toHaveCount(0);
  await ana.context.close();
  await bo.context.close();
});

test('glass-to-glass latency of a Smooth screen share stays low', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser, {}, { ana: [TIMESTAMP_SCREEN] });
  await pickInSettings(ana.page, { screen: 'smooth' });
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await shareScreen(ana, [bo]);
  await expectSenders(ana.page, { maxFramerate: 60, degradationPreference: 'maintain-framerate', contentHint: 'motion' });

  // Both pages read the same machine's clock; measure what separates them anyway.
  const clock = await clockOffset(ana.page, bo.page);
  // Known to within 5 ms, and taken out of every sample below.
  expect(clock.uncertainty).toBeLessThan(5);

  const anaKey = await selfKey(ana.page);
  const startedAt = await bo.page.evaluate(() => performance.timeOrigin + performance.now());
  await startStampDecoder(bo.page, `#tile-video-${anaKey}`);
  // Warm-up (discarded) plus ten seconds of samples.
  await expect.poll(async () => {
    const { samples } = await stampResults(bo.page);
    return samples.length ? samples[samples.length - 1].at - startedAt : 0;
  }, { timeout: 40000, intervals: [1000] }).toBeGreaterThan(13000);

  const results = await stampResults(bo.page);
  const kept = results.samples.filter((s) => s.at >= startedAt + 3000 && s.at <= startedAt + 13000);
  const latencies = kept.map((s) => s.latency - clock.offset).sort((a, b) => a - b);
  const pick = (q) => latencies[Math.min(latencies.length - 1, Math.floor(q * latencies.length))];
  const inbound = (await videoRtpStats(bo.page, 'inbound-rtp'))[0];
  const outbound = (await videoRtpStats(ana.page, 'outbound-rtp'))[0];
  console.log(
    `Latency (Smooth, ${results.mode}): median ${pick(0.5)?.toFixed(1)} ms, p95 ${pick(0.95)?.toFixed(1)} ms,`
    + ` ${latencies.length} frames in 10 s (rejected ${results.rejected}, misread ${results.misread}),`
    + ` codec ${inbound?.codec}, received ${inbound?.frameWidth}×${inbound?.frameHeight} at ${inbound?.framesPerSecond} fps,`
    + ` sender limited by ${outbound?.qualityLimitationReason}, clock offset ${clock.offset.toFixed(2)} ± ${clock.uncertainty.toFixed(2)} ms`,
  );
  expect(latencies.length).toBeGreaterThanOrEqual(50);
  expect(pick(0.5)).toBeLessThan(200);
  await ana.context.close();
  await bo.context.close();
});
