import { test, expect } from '@playwright/test';
import { createRoom, expectNoStorage, inviteFrom, joinRoom, newMember, sendMessage } from './helpers.js';

test('2-member room: ephemeral WebRTC chat, zero storage, and memory wipe', async ({ browser }) => {
  const peer1 = await newMember(browser, 'Page1');
  const peer2 = await newMember(browser, 'Page2');

  // 1. Peer 1 creates a room: room ID and key land in the URL fragment only.
  const adminUrl = await createRoom(peer1.page, { name: 'Peer One' });
  console.log('Peer 1 session URL:', adminUrl);
  await expect(peer1.page.locator('.status-indicator')).toContainText('Waiting for Peer', { timeout: 10000 });

  // 2. Peer 2 opens the invite link and picks a name.
  await joinRoom(peer2.page, inviteFrom(adminUrl), 'Peer Two');

  // 3. WebRTC P2P connection established on both sides.
  await expect(peer1.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(peer2.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 4. Messages both ways.
  const message1 = 'Hello from Peer 1 - completely ephemeral!';
  await sendMessage(peer1.page, message1);
  await expect(peer1.page.locator('.chat-container')).toContainText(message1);
  await expect(peer2.page.locator('.chat-container')).toContainText(message1, { timeout: 5000 });

  const message2 = 'Hello back from Peer 2 - verified zero-knowledge!';
  await sendMessage(peer2.page, message2);
  await expect(peer2.page.locator('.chat-container')).toContainText(message2);
  await expect(peer1.page.locator('.chat-container')).toContainText(message2, { timeout: 5000 });

  // 5. Zero persistence invariant.
  await expectNoStorage(peer1.page);
  await expectNoStorage(peer2.page);

  // 6. Reload destroys the session in memory: back to the join screen, no messages.
  await peer1.page.reload();
  await expect(peer1.page.locator('#enter-room-btn')).toBeVisible();
  await expect(peer1.page.locator('.message-bubble')).toHaveCount(0);

  await peer1.context.close();
  await peer2.context.close();
});

test('custom URL fragment params survive room creation', async ({ page }) => {
  const relay = 'ws://127.0.0.1:3333/nostr';
  await page.goto(`/#relays=${relay}&future=1`);
  await page.locator('#create-room-btn').click();

  await page.waitForFunction(() => location.hash.includes('room=') && location.hash.includes('key='));
  const hash = await page.evaluate(() => location.hash);
  expect(hash).toContain(`relays=${relay}`);
  expect(hash).toContain('future=1');

  // The custom relay is the one used for signaling.
  await expect(page.locator('.status-indicator')).toContainText('Waiting for Peer', { timeout: 10000 });
});

test('chat auto-scroll: auto-scrolls on arrival when at bottom, preserves position when scrolled up, snaps on send', async ({ browser }) => {
  const peer1 = await newMember(browser, 'Scroller1');
  const peer2 = await newMember(browser, 'Scroller2');

  const adminUrl = await createRoom(peer1.page, { name: 'Peer One' });
  await joinRoom(peer2.page, inviteFrom(adminUrl), 'Peer Two');

  await expect(peer1.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(peer2.page.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // Send enough messages to fill and overflow the chat container.
  for (let i = 1; i <= 15; i++) {
    await sendMessage(peer1.page, `Message number ${i} from Peer 1 to fill the screen`);
  }

  // Peer 2 should have received all messages and auto-scrolled to the bottom.
  await expect(peer2.page.locator('.chat-container')).toContainText('Message number 15 from Peer 1', { timeout: 10000 });

  // Verify Peer 2 is within 60px of the bottom.
  await peer2.page.waitForFunction(() => {
    const el = document.querySelector('.chat-container');
    return el && (el.scrollHeight - el.scrollTop - el.clientHeight <= 60);
  });

  // Now Peer 2 scrolls all the way up.
  await peer2.page.evaluate(() => {
    const el = document.querySelector('.chat-container');
    if (el) el.scrollTop = 0;
  });

  // Verify Peer 2's scroll position is at the top.
  const topPosition = await peer2.page.evaluate(() => {
    const el = document.querySelector('.chat-container');
    return el ? el.scrollTop : -1;
  });
  expect(topPosition).toBe(0);

  // Peer 1 sends another message while Peer 2 is scrolled up.
  await sendMessage(peer1.page, 'Message 16 while Peer 2 is scrolled up');
  await expect(peer2.page.locator('.chat-container')).toContainText('Message 16 while Peer 2', { timeout: 10000 });

  // Verify Peer 2 did NOT get auto-scrolled to the bottom: scroll position should remain near top.
  const positionAfterIncoming = await peer2.page.evaluate(() => {
    const el = document.querySelector('.chat-container');
    return el ? el.scrollTop : -1;
  });
  expect(positionAfterIncoming).toBeLessThan(100);

  // Now Peer 2 sends a message of their own: it should snap to the bottom!
  await sendMessage(peer2.page, 'Message from Peer 2 snapping down');
  await peer2.page.waitForFunction(() => {
    const el = document.querySelector('.chat-container');
    return el && (el.scrollHeight - el.scrollTop - el.clientHeight <= 60);
  });

  await peer1.context.close();
  await peer2.context.close();
});
