import { test, expect } from '@playwright/test';
import {
  AGENT, CANVAS_SCREEN, MOCK_GAMEPAD, STUB_POINTER_LOCK, TIMESTAMP_SCREEN, addInitScripts, agentEvents, createRoom,
  expectDirectMesh, expectNoStorage, inviteFrom, joinRoom, joinVoice, memberRow, newMember, pairAgent, resetAgent,
  videoSenderParams, voiceChip,
} from './helpers.js';

test.describe.configure({ timeout: 120000 });
test.beforeEach(resetAgent);
test.afterEach(resetAgent);

/**
 * Ana shares her (canvas) screen in voice; the others join voice and see it. Options:
 * `screen` replaces the 1280×720 canvas (an init script), `sharerInit` / `viewerInit` add init
 * scripts, and `beforeShare(ana)` runs once everyone is in voice.
 */
async function sharingRoom(browser, others, surface = 'monitor', { screen, sharerInit = [], viewerInit = [], beforeShare } = {}) {
  const ana = await newMember(browser, 'Ana');
  if (screen) await ana.context.addInitScript(screen);
  else await ana.context.addInitScript(CANVAS_SCREEN, { surface });
  await addInitScripts(ana, sharerInit);
  const members = [ana];
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  for (const name of others) {
    const m = await newMember(browser, name);
    await m.context.addInitScript(STUB_POINTER_LOCK);
    await m.context.addInitScript(MOCK_GAMEPAD);
    await addInitScripts(m, viewerInit);
    await joinRoom(m.page, invite, name);
    members.push(m);
  }
  const names = ['Ana', ...others];
  for (const [m, n] of members.map((m, i) => [m, names[i]])) await expectDirectMesh(m.page, names, n);
  for (const m of members) await joinVoice(m.page);
  if (beforeShare) await beforeShare(ana);
  await ana.page.locator('#screen-btn').click();
  for (const m of members.slice(1)) {
    await expect(voiceChip(m.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 15000 });
  }
  const anaKey = await ana.page.evaluate(() => window.__dchat.selfPubkey());
  return { members, anaKey };
}

const tile = (page, pk) => page.locator(`.tile[data-pubkey="${pk}"]`);

/** Init script: when the viewer sends each packet on the state lane (pointer moves), in ms. */
const RECORD_STATE_SENDS = () => {
  window.__stateSends = [];
  const send = RTCDataChannel.prototype.send;
  RTCDataChannel.prototype.send = function (data) {
    if (this.label === 'input-state') window.__stateSends.push(performance.now());
    return send.call(this, data);
  };
};

/** Init script: every JSON message the tab sends to dchat-host (`window.__agentSent`). */
const RECORD_AGENT_MESSAGES = () => {
  window.__agentSent = [];
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (data) {
    if (typeof data === 'string' && this.url.includes('127.0.0.1:7499')) {
      try {
        window.__agentSent.push(JSON.parse(data));
      } catch {
        // Not JSON: not ours to record.
      }
    }
    return send.call(this, data);
  };
};

/** Pair Ana's tab with the recording app and close the dialog. */
async function pairAndClose(ana) {
  await pairAgent(ana.page);
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await ana.page.locator('#control-host-modal .modal-title-row button').click();
}

/** The client position of a point given as fractions of the shared picture inside `pk`'s tile. */
function pictureClientPoint(page, pk, fx, fy) {
  return page.evaluate(([key, x, y]) => {
    const r = document.getElementById(`control-surface-${key}`).getBoundingClientRect();
    const video = document.getElementById(`tile-video-${key}`);
    const scale = Math.min(r.width / video.videoWidth, r.height / video.videoHeight);
    const [w, h] = [video.videoWidth * scale, video.videoHeight * scale];
    return { x: r.left + (r.width - w) / 2 + w * x, y: r.top + (r.height - h) / 2 + h * y };
  }, [pk, fx, fy]);
}

/** In the page: `count` pointer moves over `pk`'s control surface in one go, ending at `to`. */
function burstOfMoves(page, pk, to, count) {
  return page.evaluate(([key, end, n]) => {
    const surface = document.getElementById(`control-surface-${key}`);
    for (let i = n - 1; i >= 0; i -= 1) {
      surface.dispatchEvent(new PointerEvent('pointermove', {
        bubbles: true, clientX: end.x - i * 3, clientY: end.y, pointerType: 'mouse',
      }));
    }
  }, [pk, to, count]);
}

/** Wait until the recording app holds events matching `predicate`. */
async function waitForEvents(predicate, timeout = 10000) {
  await expect.poll(async () => predicate(await agentEvents()), { timeout }).toBe(true);
  return agentEvents();
}

/** Bo asks for control and Ana allows it; Bo engages by clicking the screen. */
async function takeControl(viewer, sharer, anaKey) {
  await tile(viewer.page, anaKey).locator('.control-request-btn').click();
  await expect(tile(viewer.page, anaKey)).toHaveAttribute('data-control', 'requested');
  const prompt = sharer.page.locator('.control-prompt');
  await expect(prompt).toBeVisible({ timeout: 10000 });
  await prompt.locator('.control-allow-btn').click();
  await expect(tile(viewer.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 10000 });
  await viewer.page.locator(`#control-surface-${anaKey}`).click();
  await expect(tile(viewer.page, anaKey)).toHaveAttribute('data-control', 'engaged');
}

test('pair the app, request, allow, control mouse and keyboard, release and revoke', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo']);
  await expect(tile(bo.page, anaKey).locator('.control-request-btn')).toHaveCount(0, { timeout: 2000 });

  // The dialog says where to get dchat-host (the site's configured download address).
  await ana.page.locator('#control-host-btn').click();
  const download = ana.page.locator('#agent-download-link');
  await expect(download).toHaveAttribute('href', 'https://downloads.example.test/dchat-host');
  await expect(download).toHaveAttribute('rel', 'noopener noreferrer');
  await ana.page.locator('#control-host-modal .modal-title-row button').click();

  // A wrong code is refused; the right one pairs, and control is offered.
  await pairAgent(ana.page, 'WRONG-000');
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'wrong_code', { timeout: 10000 });
  await ana.page.locator('#agent-code-input').fill(AGENT.code);
  await ana.page.locator('#agent-connect-btn').click();
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await expect(ana.page.locator('#control-host-state')).toHaveAttribute('data-hosting', 'true');
  await ana.page.locator('#control-host-modal .modal-title-row button').click();

  await takeControl(bo, ana, anaKey);
  await expect(memberRow(ana.page, 'Bo').locator('.control-badge')).toHaveText('🖱️');

  // A click in the middle of the picture lands in the middle of Ana's screen.
  const box = await bo.page.locator(`#control-surface-${anaKey}`).boundingBox();
  await bo.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await bo.page.keyboard.press('KeyA');
  let events = await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'KeyA' && !e.down));
  const down = events.find((e) => e.k === 'Button' && e.down);
  expect(down.button).toBe('left');
  const moveBeforeClick = events[events.indexOf(down) - 1];
  expect(moveBeforeClick.k).toBe('MoveAbs');
  expect(Math.abs(moveBeforeClick.x - 32768)).toBeLessThan(400);
  expect(Math.abs(moveBeforeClick.y - 32768)).toBeLessThan(400);
  expect(events.filter((e) => e.k === 'Key').map((e) => [e.code, e.down])).toEqual([['KeyA', true], ['KeyA', false]]);

  // Held keys are released when the viewer stops controlling (the shortcut).
  await bo.page.keyboard.down('ShiftLeft');
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'ShiftLeft' && e.down));
  await bo.page.keyboard.press('Control+Alt+Q');
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 5000 });
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'ShiftLeft' && !e.down));
  await bo.page.keyboard.up('ShiftLeft');

  // Ana revokes from the member list.
  await memberRow(ana.page, 'Bo').locator('.control-revoke-btn').click();
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'offer', { timeout: 10000 });
  await expect(memberRow(ana.page, 'Bo').locator('.control-badge')).toHaveCount(0);

  // Stopping the share ends the offer.
  await ana.page.locator('#screen-btn').click();
  await expect(tile(bo.page, anaKey)).toHaveCount(0, { timeout: 10000 });
  for (const m of [ana, bo]) await expectNoStorage(m.page);
  for (const m of [ana, bo]) await m.context.close();
});

test('one mouse and keyboard holder; nobody else gets input through', async ({ browser }) => {
  const { members: [ana, bo, cy], anaKey } = await sharingRoom(browser, ['Bo', 'Cy']);
  await pairAgent(ana.page);
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await ana.page.locator('#control-host-modal .modal-title-row button').click();

  // Cy, with no rights, tries to type on Ana's computer: nothing arrives.
  await cy.page.evaluate((pk) => window.__dchat.sendRawInput(pk, JSON.stringify([
    { k: 'Key', code: 'KeyZ', down: true, repeat: false },
  ])), anaKey);

  await takeControl(bo, ana, anaKey);
  await bo.page.keyboard.press('KeyB');
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'KeyB'));

  // Cy asks too; Ana sees that granting takes it from Bo.
  await tile(cy.page, anaKey).locator('.control-request-btn').click();
  const prompt = ana.page.locator('.control-prompt');
  await expect(prompt.locator('.control-prompt-note')).toContainText('Bo', { timeout: 10000 });
  await prompt.locator('.control-allow-btn').click();
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'offer', { timeout: 10000 });
  await expect(bo.page.locator('.toast')).toContainText('Someone else', { timeout: 5000 });
  await expect(tile(cy.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 10000 });
  await expect(tile(bo.page, anaKey).locator('.control-holder')).toContainText('Cy');

  // Bo's raw input no longer gets through; Cy's does.
  await bo.page.evaluate((pk) => window.__dchat.sendRawInput(pk, JSON.stringify([
    { k: 'Key', code: 'KeyX', down: true, repeat: false },
  ])), anaKey);
  await cy.page.locator(`#control-surface-${anaKey}`).click();
  await cy.page.keyboard.press('KeyC');
  const events = await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'KeyC' && !e.down));
  expect(events.some((e) => e.k === 'Key' && (e.code === 'KeyZ' || e.code === 'KeyX'))).toBe(false);

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('prompts show in fullscreen; losing the link releases held keys; window shares are not offered', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo']);
  await pairAgent(ana.page);
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await ana.page.locator('#control-host-modal .modal-title-row button').click();

  // Ana watches the grid in fullscreen: Bo's request still shows up there.
  await ana.page.locator('#fullscreen-btn').click();
  await expect.poll(() => ana.page.evaluate(() => document.fullscreenElement?.id)).toBe('video-grid');
  await tile(bo.page, anaKey).locator('.control-request-btn').click();
  await expect(ana.page.locator('.control-prompt')).toBeVisible({ timeout: 10000 });
  expect(await ana.page.evaluate(() => document.fullscreenElement.contains(document.getElementById('control-prompts')))).toBe(true);
  await ana.page.locator('.control-allow-btn').click();
  await ana.page.evaluate(() => document.exitFullscreen());

  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 10000 });
  await bo.page.locator(`#control-surface-${anaKey}`).click();
  await bo.page.keyboard.down('ControlLeft');
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'ControlLeft' && e.down));

  // The link between them drops: Ana's tab revokes Bo and the app lets go of the key.
  const boKey = await bo.page.evaluate(() => window.__dchat.selfPubkey());
  await ana.page.evaluate((pk) => window.__dchat.blockPeer(pk), boKey);
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'ControlLeft' && !e.down));
  await bo.page.keyboard.up('ControlLeft');
  for (const m of [ana, bo]) await m.context.close();

  // Sharing a window instead of a whole screen: control is not offered.
  const second = await sharingRoom(browser, ['Bo'], 'window');
  const [ana2, bo2] = second.members;
  await pairAgent(ana2.page);
  await expect(ana2.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await expect(ana2.page.locator('#control-host-state')).toHaveAttribute('data-hosting', 'false');
  await bo2.page.waitForTimeout(1500);
  await expect(tile(bo2.page, second.anaKey).locator('.control-request-btn')).toHaveCount(0);
  for (const m of [ana2, bo2]) await m.context.close();
});

test('game mode: pointer lock, relative movement, video left on the preset, and losing the lock ends control', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo']);
  await pairAgent(ana.page);
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await ana.page.locator('#control-host-modal .modal-title-row button').click();
  await tile(bo.page, anaKey).locator('.control-request-btn').click();
  await ana.page.locator('.control-allow-btn').click({ timeout: 10000 });
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 10000 });

  // Switch to game mode, then engage: the pointer is locked and moves are relative.
  const modeButton = tile(bo.page, anaKey).locator('.control-mode-btn');
  await expect(modeButton).toHaveAttribute('data-mode', 'desktop');
  await modeButton.click();
  await expect(modeButton).toHaveAttribute('data-mode', 'game');
  const box = await bo.page.locator(`#control-surface-${anaKey}`).boundingBox();
  await bo.page.mouse.move(box.x + 100, box.y + 100);
  await bo.page.locator(`#control-surface-${anaKey}`).click({ position: { x: 100, y: 100 } });
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'engaged');
  expect(await bo.page.evaluate(() => window.__pointerLockRequests)).toBe(1);

  await bo.page.mouse.move(box.x + 150, box.y + 120, { steps: 5 });
  const events = await waitForEvents((ev) => {
    const rel = ev.filter((e) => e.k === 'MoveRel');
    return rel.reduce((sum, e) => sum + e.dx, 0) >= 50;
  });
  const rel = events.filter((e) => e.k === 'MoveRel');
  expect(rel.reduce((sum, e) => sum + e.dx, 0)).toBe(50);
  expect(rel.reduce((sum, e) => sum + e.dy, 0)).toBe(20);
  expect(events.some((e) => e.k === 'MoveAbs')).toBe(false);
  // Game mode leaves the video alone: the sharer's preset (Balanced) stays in charge.
  await expect.poll(() => ana.page.evaluate(() => window.__pcs
    .filter((pc) => pc.connectionState === 'connected')
    .flatMap((pc) => pc.getSenders())
    .filter((s) => s.track && s.track.kind === 'video')
    .map((s) => ({ hint: s.track.contentHint, maxFramerate: s.getParameters().encodings[0]?.maxFramerate }))))
    .toEqual([{ hint: '', maxFramerate: 30 }]);

  // Losing the pointer lock (Esc in a real browser) ends control.
  await bo.page.evaluate(() => document.exitPointerLock());
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 5000 });
  for (const m of [ana, bo]) await m.context.close();
});

test('controllers: several members at once, each in their own slot, released on revoke', async ({ browser }) => {
  const { members: [ana, bo, cy], anaKey } = await sharingRoom(browser, ['Bo', 'Cy']);
  await pairAgent(ana.page);
  await expect(ana.page.locator('#agent-status')).toHaveAttribute('data-status', 'paired', { timeout: 10000 });
  await ana.page.locator('#control-host-modal .modal-title-row button').click();

  // Bo and Cy each ask for a controller; Ana allows both.
  for (const viewer of [bo, cy]) {
    await tile(viewer.page, anaKey).locator('.control-request-pad-btn').click();
    const prompt = ana.page.locator('.control-prompt');
    await expect(prompt).toContainText('controller', { timeout: 10000 });
    await prompt.locator('.control-allow-btn').click();
    await expect(tile(viewer.page, anaKey)).toHaveAttribute('data-control', 'pad', { timeout: 10000 });
  }
  await expect(tile(bo.page, anaKey).locator('.control-pad-badge')).toHaveText('🎮 P1');
  await expect(tile(cy.page, anaKey).locator('.control-pad-badge')).toHaveText('🎮 P2');
  await expect(memberRow(ana.page, 'Cy').locator('.control-badge')).toHaveText('🎮P2');
  let events = await waitForEvents((ev) => ev.filter((e) => e.k === 'PadPlug').length === 2);
  expect(events.filter((e) => e.k === 'PadPlug').map((e) => e.slot).sort()).toEqual([0, 1]);

  // A on Bo's controller presses A on P1; Cy's stick moves P2.
  await bo.page.evaluate(() => window.__pressPad(0, true));
  await cy.page.evaluate(() => window.__tiltPad(0, 1));
  events = await waitForEvents((ev) =>
    ev.some((e) => e.k === 'PadUpdate' && e.slot === 0 && e.state.buttons === 1)
    && ev.some((e) => e.k === 'PadUpdate' && e.slot === 1 && e.state.axes[0] === 32767));
  expect(events.some((e) => e.k === 'PadUpdate' && e.slot === 0 && e.state.axes[0] !== 0)).toBe(false);
  expect(events.some((e) => e.k === 'Key' || e.k === 'MoveAbs')).toBe(false);

  // Revoking Cy puts P2 back to neutral and unplugs it; Bo keeps P1.
  await memberRow(ana.page, 'Cy').locator('.control-revoke-btn').click();
  events = await waitForEvents((ev) => ev.some((e) => e.k === 'PadUnplug' && e.slot === 1));
  const unplug = events.findIndex((e) => e.k === 'PadUnplug' && e.slot === 1);
  expect(events.slice(0, unplug).reverse().find((e) => e.k === 'PadUpdate' && e.slot === 1).state)
    .toEqual({ buttons: 0, axes: [0, 0, 0, 0], triggers: [0, 0] });
  await expect(tile(cy.page, anaKey)).toHaveAttribute('data-control', 'offer', { timeout: 10000 });
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'pad');

  // Bo can also take mouse and keyboard while keeping the controller.
  await tile(bo.page, anaKey).locator('.control-request-btn').click();
  await ana.page.locator('.control-prompt .control-allow-btn').click({ timeout: 10000 });
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 10000 });
  await expect(tile(bo.page, anaKey).locator('.control-pad-badge')).toHaveText('🎮 P1');
  for (const m of [ana, bo, cy]) await m.context.close();
});

test('with the stats panel open over the picture, control clicks still land', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo']);
  await pairAndClose(ana);
  await tile(bo.page, anaKey).locator('.tile-stats-btn').click();
  const panel = tile(bo.page, anaKey).locator('.tile-stats');
  await expect(panel).toHaveAttribute('data-state', 'ready', { timeout: 10000 });

  await takeControl(bo, ana, anaKey);
  await expect(panel).toBeVisible();
  // A click right on the panel goes through it to Ana's screen (top right of it).
  const box = await panel.boundingBox();
  await bo.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  const events = await waitForEvents((ev) => ev.some((e) => e.k === 'Button' && !e.down));
  const down = events.find((e) => e.k === 'Button' && e.down);
  expect(down.button).toBe('left');
  const move = events[events.indexOf(down) - 1];
  expect(move.k).toBe('MoveAbs');
  expect(move.x).toBeGreaterThan(32768);
  expect(move.y).toBeLessThan(32768);
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'engaged');
  await expect(panel).toBeVisible();
  for (const m of [ana, bo]) await m.context.close();
});

test('no pointer move reaches the computer after the release shortcut, even one still waiting', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo']);
  await pairAndClose(ana);
  await takeControl(bo, ana, anaKey);

  // A held key marks the release in the recording: only ReleaseAll lets go of it.
  await bo.page.keyboard.down('KeyH');
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'KeyH' && e.down));
  const box = await bo.page.locator(`#control-surface-${anaKey}`).boundingBox();
  await bo.page.mouse.move(box.x + 20, box.y + 20);
  await bo.page.mouse.move(box.x + box.width - 20, box.y + box.height - 20, { steps: 20 });
  // Then, in one go: moves (all but the first wait for the 4 ms gap) and the release chord.
  const to = await pictureClientPoint(bo.page, anaKey, 0.3, 0.3);
  await bo.page.evaluate(([key, end]) => {
    const surface = document.getElementById(`control-surface-${key}`);
    for (let i = 0; i < 5; i += 1) {
      surface.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, clientX: end.x + i, clientY: end.y, pointerType: 'mouse' }));
    }
    window.dispatchEvent(new KeyboardEvent('keydown', { code: 'KeyQ', key: 'Q', ctrlKey: true, altKey: true, shiftKey: true }));
  }, [anaKey, to]);
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 5000 });
  await waitForEvents((ev) => ev.some((e) => e.k === 'Key' && e.code === 'KeyH' && !e.down));
  // Long enough for a trailing move, or a late packet, to show up.
  await bo.page.waitForTimeout(1000);
  const events = await agentEvents();
  const released = events.findIndex((e) => e.k === 'Key' && e.code === 'KeyH' && !e.down);
  expect(events.slice(0, released).some((e) => e.k === 'MoveAbs')).toBe(true);
  expect(events.slice(released).filter((e) => e.k === 'MoveAbs' || e.k === 'MoveRel')).toEqual([]);
  await bo.page.keyboard.up('KeyH');
  for (const m of [ana, bo]) await m.context.close();
});

test('pointer moves go out at once, at least 4 ms apart, and the last position always arrives', async ({ browser }) => {
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo'], 'monitor', { viewerInit: [RECORD_STATE_SENDS] });
  await pairAndClose(ana);
  await takeControl(bo, ana, anaKey);
  const nearCenter = (e) => e && Math.abs(e.x - 32768) < 400 && Math.abs(e.y - 32768) < 400;
  const lastMove = (ev) => ev.filter((e) => e.k === 'MoveAbs').at(-1);

  // A real mouse gliding to the middle of the picture.
  const box = await bo.page.locator(`#control-surface-${anaKey}`).boundingBox();
  await bo.page.mouse.move(box.x + 10, box.y + 10);
  await bo.page.evaluate(() => {
    window.__stateSends.length = 0;
  });
  await bo.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 40 });
  await waitForEvents((ev) => nearCenter(lastMove(ev)));
  const sends = await bo.page.evaluate(() => window.__stateSends);
  expect(sends.length).toBeGreaterThan(0);
  expect(sends.length).toBeLessThanOrEqual(40);
  const gaps = sends.slice(1).map((t, i) => t - sends[i]);
  // `Date.now()` counts whole milliseconds, so 4 ms apart is never less than 3 measured here.
  for (const gap of gaps) expect(gap).toBeGreaterThanOrEqual(3);

  // Twenty moves in one go: the first leaves at once, the rest collapse into one trailing
  // move to where the pointer stopped.
  await bo.page.evaluate(() => {
    window.__stateSends.length = 0;
  });
  const to = await pictureClientPoint(bo.page, anaKey, 0.25, 0.25);
  await burstOfMoves(bo.page, anaKey, to, 20);
  const events = await waitForEvents((ev) => {
    const last = lastMove(ev);
    return Boolean(last) && Math.abs(last.x - 16384) < 400 && Math.abs(last.y - 16384) < 400;
  });
  expect(await bo.page.evaluate(() => window.__stateSends.length)).toBe(2);
  expect(events.filter((e) => e.k === 'Button' || e.k === 'Key')).toEqual([]);
  for (const m of [ana, bo]) await m.context.close();
});

test('dchat-host is told the source size of the shared screen under every preset', async ({ browser }) => {
  const pick = async (page, preset) => {
    await page.locator('#screen-quality-btn').click();
    await page.locator(`#screen-quality-menu .quality-option[data-preset="${preset}"]`).click();
    await expect(page.locator('#screen-quality-menu')).toHaveCount(0);
  };
  const { members: [ana, bo], anaKey } = await sharingRoom(browser, ['Bo'], 'monitor', {
    screen: TIMESTAMP_SCREEN,
    sharerInit: [RECORD_AGENT_MESSAGES],
    beforeShare: (sharer) => pick(sharer.page, 'fastest'),
  });
  await pairAndClose(ana);
  const screens = () => ana.page.evaluate(() => window.__agentSent.filter((m) => m.t === 'Screen'));
  await expect.poll(async () => (await screens()).length, { timeout: 10000 }).toBeGreaterThan(0);
  // Fastest sends at most 1280 px wide, yet the app hears of the whole 1920×1080 monitor.
  await expect.poll(async () => {
    const sender = (await videoSenderParams(ana.page))[0];
    return sender && sender.maxFramerate === 60 && sender.width / sender.scaleResolutionDownBy <= 1280;
  }, { timeout: 15000 }).toBe(true);
  expect((await screens()).at(-1)).toMatchObject({ surface: 'monitor', width: 1920, height: 1080 });

  for (const preset of ['smooth', 'balanced', 'sharp', 'text']) {
    await ana.page.locator('#screen-btn').click();
    await expect(tile(bo.page, anaKey)).toHaveCount(0, { timeout: 10000 });
    await pick(ana.page, preset);
    const before = (await screens()).length;
    await ana.page.locator('#screen-btn').click();
    await expect(tile(bo.page, anaKey).locator('.control-request-btn')).toBeVisible({ timeout: 15000 });
    await expect.poll(async () => (await screens()).length, { timeout: 10000 }).toBeGreaterThan(before);
    expect((await screens()).at(-1)).toMatchObject({ surface: 'monitor', width: 1920, height: 1080 });
  }
  for (const m of [ana, bo]) await m.context.close();
});
