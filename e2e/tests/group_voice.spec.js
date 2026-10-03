import { test, expect } from '@playwright/test';
import {
  createRoom,
  expectAudioFrom,
  expectDirectMesh,
  expectVideoFrames,
  inviteFrom,
  joinRoom,
  joinVoice,
  newMember,
  voiceChip,
} from './helpers.js';

test.describe.configure({ timeout: 120000 });

async function threeMembers(browser, createOptions = {}) {
  const ana = await newMember(browser, 'Ana');
  const bo = await newMember(browser, 'Bo');
  const cy = await newMember(browser, 'Cy');
  const invite = inviteFrom(await createRoom(ana.page, { name: 'Ana', ...createOptions }));
  await joinRoom(bo.page, invite, 'Bo');
  await joinRoom(cy.page, invite, 'Cy');
  for (const [m, name] of [[ana, 'Ana'], [bo, 'Bo'], [cy, 'Cy']]) {
    await expectDirectMesh(m.page, ['Ana', 'Bo', 'Cy'], name);
  }
  return [ana, bo, cy];
}

test('drop-in lounge: join prompt, mesh audio, mute state, speaking indicator, leave', async ({ browser }) => {
  const [ana, bo, cy] = await threeMembers(browser);

  // Ana drops in; nobody is rung, but members outside voice get a prompt.
  await joinVoice(ana.page);
  await expect(voiceChip(cy.page, 'Ana')).toBeVisible({ timeout: 10000 });
  await expect(bo.page.locator('#voice-prompt')).toContainText('Ana joined voice', { timeout: 10000 });
  await bo.page.locator('#voice-prompt-join').click();
  await expect(bo.page.locator('#leave-voice-btn')).toBeVisible({ timeout: 10000 });
  await joinVoice(cy.page);

  for (const m of [ana, bo, cy]) {
    await expect(m.page.locator('.voice-chip')).toHaveCount(3, { timeout: 10000 });
  }
  // Every member hears the other two over their direct links.
  for (const m of [ana, bo, cy]) await expectAudioFrom(m.page, 2);
  await expect(ana.page.locator('audio.remote-audio')).toHaveCount(2);

  // Mute state is shared with the room.
  await bo.page.locator('#mic-btn').click();
  await expect(voiceChip(ana.page, 'Bo')).toHaveAttribute('data-mic', 'off', { timeout: 10000 });
  await expect(voiceChip(cy.page, 'Bo')).toHaveAttribute('data-mic', 'off', { timeout: 10000 });

  // Chromium's fake microphone beeps: the speaking highlight follows the audio level.
  await expect(voiceChip(ana.page, 'Cy')).toHaveAttribute('data-speaking', 'true', { timeout: 15000 });
  await expect(voiceChip(ana.page, 'Bo')).toHaveAttribute('data-speaking', 'false');

  // Leaving frees the seat for everyone.
  await cy.page.locator('#leave-voice-btn').click();
  await expect(cy.page.locator('#join-voice-btn')).toBeVisible();
  await expect(ana.page.locator('.voice-chip')).toHaveCount(2, { timeout: 10000 });
  await expect(bo.page.locator('.voice-chip')).toHaveCount(2, { timeout: 10000 });

  for (const m of [ana, bo, cy]) await m.context.close();
});

test('voice and video caps are enforced per room', async ({ browser }) => {
  const [ana, bo, cy] = await threeMembers(browser, { voiceCap: 2, videoCap: 1 });

  await joinVoice(ana.page);
  await joinVoice(bo.page);
  await expect(cy.page.locator('.voice-chip')).toHaveCount(2, { timeout: 10000 });
  await expect(cy.page.locator('#join-voice-btn')).toBeDisabled();
  await expect(cy.page.locator('#join-voice-btn')).toHaveText('Voice is full');
  await expect(cy.page.locator('.lounge-count')).toHaveText('2/2');

  // One video slot: Ana takes it, Bo's camera and screen buttons are disabled.
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'camera', { timeout: 10000 });
  await expectVideoFrames(bo.page, 'Ana');
  await expect(bo.page.locator('#camera-btn')).toBeDisabled();
  await expect(bo.page.locator('#screen-btn')).toBeDisabled();

  // Ana turns the camera off: the slot frees up.
  await ana.page.locator('#camera-btn').click();
  await expect(voiceChip(bo.page, 'Ana')).toHaveAttribute('data-video', 'none', { timeout: 10000 });
  await expect(bo.page.locator('#camera-btn')).toBeEnabled();

  for (const m of [ana, bo, cy]) await m.context.close();
});
