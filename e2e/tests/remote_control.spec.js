import { test, expect } from '@playwright/test';
import {
  AGENT, CANVAS_SCREEN, STUB_POINTER_LOCK, agentEvents, createRoom, expectDirectMesh, expectNoStorage, inviteFrom,
  joinRoom, joinVoice, memberRow, newMember, pairAgent, resetAgent, voiceChip,
} from './helpers.js';

test.describe.configure({ timeout: 120000 });
test.beforeEach(resetAgent);
test.afterEach(resetAgent);

/** Ana shares her (canvas) screen in voice; the others join voice and see it. */
async function sharingRoom(browser, others, surface = 'monitor') {
  const ana = await newMember(browser, 'Ana');
  await ana.context.addInitScript(CANVAS_SCREEN, { surface });
  const members = [ana];
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana' }));
  for (const name of others) {
    const m = await newMember(browser, name);
    await m.context.addInitScript(STUB_POINTER_LOCK);
    await joinRoom(m.page, invite, name);
    members.push(m);
  }
  const names = ['Ana', ...others];
  for (const [m, n] of members.map((m, i) => [m, names[i]])) await expectDirectMesh(m.page, names, n);
  for (const m of members) await joinVoice(m.page);
  await ana.page.locator('#screen-btn').click();
  for (const m of members.slice(1)) {
    await expect(voiceChip(m.page, 'Ana')).toHaveAttribute('data-video', 'screen', { timeout: 15000 });
  }
  const anaKey = await ana.page.evaluate(() => window.__dchat.selfPubkey());
  return { members, anaKey };
}

const tile = (page, pk) => page.locator(`.tile[data-pubkey="${pk}"]`);

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

test('game mode: pointer lock, relative movement, smooth video, and losing the lock ends control', async ({ browser }) => {
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
  // The sharer's stream switches to smooth motion for games.
  await expect.poll(() => ana.page.evaluate(() => window.__pcs
    .flatMap((pc) => pc.getSenders()).map((s) => s.track).find((t) => t && t.kind === 'video')?.contentHint)).toBe('motion');

  // Losing the pointer lock (Esc in a real browser) ends control.
  await bo.page.evaluate(() => document.exitPointerLock());
  await expect(tile(bo.page, anaKey)).toHaveAttribute('data-control', 'granted', { timeout: 5000 });
  for (const m of [ana, bo]) await m.context.close();
});
