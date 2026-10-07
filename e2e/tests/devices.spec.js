import { test, expect } from '@playwright/test';
import {
  addInitScripts, createRoom, expectNoStorage, inviteFrom, joinRoom, joinVoice, newMember, twoMembers, videoSenderParams,
} from './helpers.js';

// As in playwright.config.js, with two fake cameras (Chromium lists one by default). Its fake
// devices: microphones "Fake Audio Input 1/2", speakers "Fake Audio Output 1/2" (plus the
// "default" aliases, which the pickers leave out) and cameras fake_device_0/1.
test.use({
  launchOptions: {
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream=device-count=2',
      '--autoplay-policy=user-gesture-required',
      '--auto-select-desktop-capture-source=Entire screen',
    ],
  },
});

const PICKERS = ['#mic-device-select', '#speaker-device-select', '#camera-device-select'];

/** Init script: the browser also lists a microphone and a camera that are gone when opened. */
const UNPLUGGED_DEVICES = () => {
  const enumerate = MediaDevices.prototype.enumerateDevices;
  MediaDevices.prototype.enumerateDevices = async function enumerateDevices() {
    const listed = await enumerate.call(this);
    return [
      ...listed,
      { kind: 'audioinput', deviceId: 'unplugged-mic', label: 'Unplugged Mic', groupId: 'gone' },
      { kind: 'videoinput', deviceId: 'unplugged-cam', label: 'Unplugged Camera', groupId: 'gone' },
    ];
  };
};

async function openSettings(page) {
  await page.locator('#audio-settings-btn').click();
  await expect(page.locator('#mic-device-select')).toBeVisible();
}

async function closeSettings(page) {
  await page.locator('.modal-content button:has-text("Close")').click();
  await expect(page.locator('#mic-device-select')).toHaveCount(0);
}

/** A picker's options as `[value, label]`, once the browser's list has arrived. */
async function pickerOptions(page, selector, count) {
  await expect(page.locator(`${selector} option`)).toHaveCount(count);
  return page.locator(`${selector} option`).evaluateAll((options) => options.map((o) => [o.value, o.textContent]));
}

/** The track of `kind` that `page` sends on its first open link, with its device. */
const sent = (page, kind) => page.evaluate((k) => {
  const pc = window.__pcs.find((p) => p.connectionState === 'connected');
  const track = pc?.getSenders().find((s) => s.track?.kind === k)?.track;
  if (!track || track.readyState !== 'live') return null;
  return { id: track.id, enabled: track.enabled, deviceId: track.getSettings().deviceId };
}, kind);

const sentDevice = async (page, kind) => (await sent(page, kind))?.deviceId ?? null;

/** `sinkId` of every member's hidden `<audio>`. */
const sinkIds = (page) => page.locator('audio.remote-audio').evaluateAll((els) => els.map((a) => a.sinkId));

test('device pickers list microphones, speakers and cameras; choices are RAM only', async ({ browser }) => {
  const ana = await newMember(browser, 'Ana');
  await createRoom(ana.page, { name: 'Ana' });
  await openSettings(ana.page);

  // Devices come first, each picker starting at "System default", then the browser's devices.
  await expect(ana.page.locator('.audio-settings-modal .settings-section').first()).toHaveText('🎧 Devices');
  const mics = await pickerOptions(ana.page, '#mic-device-select', 3);
  expect(mics.map(([, label]) => label)).toEqual(['System default', 'Fake Audio Input 1', 'Fake Audio Input 2']);
  expect(mics[0][0]).toBe('');
  const speakers = await pickerOptions(ana.page, '#speaker-device-select', 3);
  expect(speakers.map(([, label]) => label)).toEqual(['System default', 'Fake Audio Output 1', 'Fake Audio Output 2']);
  const cameras = await pickerOptions(ana.page, '#camera-device-select', 3);
  expect(cameras.map(([, label]) => label)).toEqual(['System default', 'fake_device_0', 'fake_device_1']);
  for (const id of PICKERS) await expect(ana.page.locator(id)).toHaveValue('');
  // The browser names its devices here: no hint, and no chooser button (Chromium lists speakers).
  await expect(ana.page.locator('.device-pickers .settings-hint')).toHaveCount(0);
  await expect(ana.page.locator('#speaker-choose-btn')).toHaveCount(0);

  // Choices stay across closing the settings...
  await ana.page.locator('#mic-device-select').selectOption(mics[2][0]);
  await ana.page.locator('#speaker-device-select').selectOption(speakers[1][0]);
  await ana.page.locator('#camera-device-select').selectOption(cameras[2][0]);
  await closeSettings(ana.page);
  await openSettings(ana.page);
  await expect(ana.page.locator('#mic-device-select')).toHaveValue(mics[2][0]);
  await expect(ana.page.locator('#speaker-device-select')).toHaveValue(speakers[1][0]);
  await expect(ana.page.locator('#camera-device-select')).toHaveValue(cameras[2][0]);
  // ...and "System default" goes back to none.
  await ana.page.locator('#speaker-device-select').selectOption('');
  await expect(ana.page.locator('#speaker-device-select')).toHaveValue('');

  // ...but live in RAM only: nothing stored, a reload starts from the defaults.
  await expectNoStorage(ana.page);
  await ana.page.reload();
  await ana.page.locator('#enter-room-btn').click();
  await openSettings(ana.page);
  await pickerOptions(ana.page, '#mic-device-select', 3);
  for (const id of PICKERS) await expect(ana.page.locator(id)).toHaveValue('');
  await expectNoStorage(ana.page);
  await ana.context.close();
});

test('a chosen microphone is swapped in live (keeping the mute) and kept after rejoining voice', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect.poll(() => sent(ana.page, 'audio'), { timeout: 10000 }).not.toBeNull();
  const first = await sent(ana.page, 'audio');

  // Muted, then another microphone: a new track from that device, still muted.
  await ana.page.locator('#mic-btn').click();
  await openSettings(ana.page);
  const mics = await pickerOptions(ana.page, '#mic-device-select', 3);
  const [mic1, mic2] = [mics[1][0], mics[2][0]];
  await ana.page.locator('#mic-device-select').selectOption(mic2);
  await expect.poll(() => sentDevice(ana.page, 'audio'), { timeout: 10000 }).toBe(mic2);
  const swapped = await sent(ana.page, 'audio');
  expect(swapped.id).not.toBe(first.id);
  expect(swapped.enabled).toBe(false);

  // Quick changes: the last one wins.
  await ana.page.locator('#mic-device-select').selectOption(mic1);
  await ana.page.locator('#mic-device-select').selectOption(mic2);
  await ana.page.waitForTimeout(1000);
  await expect.poll(() => sentDevice(ana.page, 'audio'), { timeout: 10000 }).toBe(mic2);
  await closeSettings(ana.page);
  await expect(ana.page.locator('#mic-btn')).toHaveClass(/muted/);

  // Leaving and joining voice again opens the same microphone.
  await ana.page.locator('#leave-voice-btn').click();
  await joinVoice(ana.page);
  await expect.poll(() => sentDevice(ana.page, 'audio'), { timeout: 10000 }).toBe(mic2);

  // Back to the system default.
  await openSettings(ana.page);
  await ana.page.locator('#mic-device-select').selectOption('');
  await expect.poll(() => sentDevice(ana.page, 'audio'), { timeout: 10000 }).not.toBe(mic2);
  await ana.context.close();
  await bo.context.close();
});

test('a chosen camera replaces the live one with the preset kept; 🔄 moves to the next camera', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  await openSettings(ana.page);
  await ana.page.locator('#camera-preset-hd').check();
  const cameras = await pickerOptions(ana.page, '#camera-device-select', 3);
  const [cam0, cam1] = [cameras[1][0], cameras[2][0]];
  await closeSettings(ana.page);
  await joinVoice(ana.page);
  await joinVoice(bo.page);

  // Camera on with the system default: the 🔄 button flips front/rear.
  await ana.page.locator('#camera-btn').click();
  await expect.poll(() => sentDevice(ana.page, 'video'), { timeout: 15000 }).toBe(cam0);
  await expect(ana.page.locator('#flip-camera-btn')).toHaveAttribute('title', 'Flip Front/Rear Camera');
  const hd = { hasTrack: true, width: 1280, height: 720, maxFramerate: 30, degradationPreference: 'balanced' };
  const params = async () => (await videoSenderParams(ana.page)).map((p) => p && Object.fromEntries(Object.keys(hd).map((k) => [k, p[k]])));
  await expect.poll(params, { timeout: 15000 }).toEqual([hd]);
  const [{ contentHint }] = await videoSenderParams(ana.page);
  const audioBefore = await sent(ana.page, 'audio');

  // Another camera while live: the sent track comes from it, still HD, the mic untouched.
  await openSettings(ana.page);
  await ana.page.locator('#camera-device-select').selectOption(cam1);
  await expect.poll(() => sentDevice(ana.page, 'video'), { timeout: 15000 }).toBe(cam1);
  await expect.poll(params, { timeout: 15000 }).toEqual([hd]);
  expect((await videoSenderParams(ana.page))[0].contentHint).toBe(contentHint);
  expect((await sent(ana.page, 'audio')).id).toBe(audioBefore.id);
  await closeSettings(ana.page);

  // With a camera chosen, 🔄 goes to the next one (wrapping) and the settings follow.
  await expect(ana.page.locator('#flip-camera-btn')).toHaveAttribute('title', 'Switch to the next camera');
  await ana.page.locator('#flip-camera-btn').click();
  await expect.poll(() => sentDevice(ana.page, 'video'), { timeout: 15000 }).toBe(cam0);
  await expect.poll(params, { timeout: 15000 }).toEqual([hd]);
  await openSettings(ana.page);
  await expect(ana.page.locator('#camera-device-select')).toHaveValue(cam0);
  await closeSettings(ana.page);

  // Off and on again: the chosen camera.
  await ana.page.locator('#camera-btn').click();
  await expect.poll(() => sent(ana.page, 'video'), { timeout: 10000 }).toBeNull();
  await ana.page.locator('#camera-btn').click();
  await expect.poll(() => sentDevice(ana.page, 'video'), { timeout: 15000 }).toBe(cam0);
  await ana.context.close();
  await bo.context.close();
});

test('a chosen speaker plays every member, including members who join voice later', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser);
  const cy = await newMember(browser, 'Cy');
  await openSettings(ana.page);
  const outputs = await ana.page.locator('#speaker-device-select option').count();
  test.skip(outputs < 3, `This browser lists ${Math.max(0, outputs - 1)} speaker(s): setSinkId can't be exercised here.`);
  const speakers = await pickerOptions(ana.page, '#speaker-device-select', 3);
  const [out1, out2] = [speakers[1][0], speakers[2][0]];

  // Chosen before anyone is heard: the audio element made for Bo plays on it.
  await ana.page.locator('#speaker-device-select').selectOption(out1);
  await closeSettings(ana.page);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect(ana.page.locator('audio.remote-audio')).toHaveCount(1, { timeout: 15000 });
  await expect.poll(() => sinkIds(ana.page), { timeout: 10000 }).toEqual([out1]);

  // Changed while hearing Bo: his element moves; Cy, joining later, gets it too.
  await openSettings(ana.page);
  await ana.page.locator('#speaker-device-select').selectOption(out2);
  await closeSettings(ana.page);
  await expect.poll(() => sinkIds(ana.page), { timeout: 10000 }).toEqual([out2]);
  await joinRoom(cy.page, inviteFrom(ana.page.url()), 'Cy');
  await joinVoice(cy.page);
  await expect(ana.page.locator('audio.remote-audio')).toHaveCount(2, { timeout: 20000 });
  await expect.poll(() => sinkIds(ana.page), { timeout: 10000 }).toEqual([out2, out2]);

  // "System default" again.
  await openSettings(ana.page);
  await ana.page.locator('#speaker-device-select').selectOption('');
  await expect.poll(() => sinkIds(ana.page), { timeout: 10000 }).toEqual(['', '']);
  for (const m of [ana, bo, cy]) await m.context.close();
});

test('a chosen microphone or camera that is gone falls back to the system default with a toast', async ({ browser }) => {
  test.setTimeout(90000);
  const [ana, bo] = await twoMembers(browser, {}, { ana: [UNPLUGGED_DEVICES] });

  // Chosen out of voice: joining opens the default microphone instead, and says so.
  await openSettings(ana.page);
  await pickerOptions(ana.page, '#mic-device-select', 4);
  await ana.page.locator('#mic-device-select').selectOption('unplugged-mic');
  await closeSettings(ana.page);
  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect(ana.page.locator('.toast')).toHaveText("The chosen microphone isn't available: using the system default.");
  await expect.poll(() => sentDevice(ana.page, 'audio'), { timeout: 10000 }).not.toBeNull();
  expect(await sentDevice(ana.page, 'audio')).not.toBe('unplugged-mic');
  await openSettings(ana.page);
  await expect(ana.page.locator('#mic-device-select')).toHaveValue('');

  // Chosen in voice: the mic is swapped for the default, still live.
  const before = await sent(ana.page, 'audio');
  await ana.page.locator('#mic-device-select').selectOption('unplugged-mic');
  await expect(ana.page.locator('.toast')).toHaveText("The chosen microphone isn't available: using the system default.");
  await expect(ana.page.locator('#mic-device-select')).toHaveValue('');
  await expect.poll(async () => (await sent(ana.page, 'audio'))?.id, { timeout: 10000 }).not.toBe(before.id);
  expect(await sentDevice(ana.page, 'audio')).not.toBe('unplugged-mic');
  await closeSettings(ana.page);

  // The camera, chosen while it is live.
  await ana.page.locator('#camera-btn').click();
  await expect.poll(() => sentDevice(ana.page, 'video'), { timeout: 15000 }).not.toBeNull();
  const camera = await sent(ana.page, 'video');
  await openSettings(ana.page);
  await ana.page.locator('#camera-device-select').selectOption('unplugged-cam');
  await expect(ana.page.locator('.toast')).toHaveText("The chosen camera isn't available: using the system default.");
  await expect(ana.page.locator('#camera-device-select')).toHaveValue('');
  await expect.poll(async () => (await sent(ana.page, 'video'))?.id, { timeout: 10000 }).not.toBe(camera.id);
  expect(await sentDevice(ana.page, 'video')).toBe(camera.deviceId);
  await expectNoStorage(ana.page);
  await ana.context.close();
  await bo.context.close();
});
