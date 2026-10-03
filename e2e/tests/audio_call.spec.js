import { test, expect } from '@playwright/test';

test('2-peer audio call handshake, mute toggle, and end call', async ({ browser }) => {
  const context1 = await browser.newContext({
    permissions: ['microphone'],
  });
  const context2 = await browser.newContext({
    permissions: ['microphone'],
  });

  const initScript = `
    window.__pcs = [];
    const OrigPC = window.RTCPeerConnection;
    window.RTCPeerConnection = function(...args) {
      const pc = new OrigPC(...args);
      window.__pcs.push(pc);
      return pc;
    };
    window.RTCPeerConnection.prototype = OrigPC.prototype;
  `;

  await context1.addInitScript(initScript);
  await context2.addInitScript(initScript);

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  page1.on('console', msg => console.log('[P1]', msg.text()));
  page2.on('console', msg => console.log('[P2]', msg.text()));
  page1.on('pageerror', err => console.log('[P1 ERR]', err));
  page2.on('pageerror', err => console.log('[P2 ERR]', err));

  // 1. Peer 1 opens app
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();

  // 2. Peer 2 opens the exact room URL
  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  // 3. Wait for P2P connection
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 4. Peer 1 clicks "📞 Audio"
  const callBtn1 = page1.locator('button:has-text("📞 Audio")');
  await expect(callBtn1).toBeVisible();
  await callBtn1.click();

  // Peer 1 should see "Calling Peer with Audio"
  await expect(page1.locator('.call-bar')).toContainText('Calling Peer with Audio', { timeout: 5000 });

  // Peer 2 should see incoming call prompt with Accept button
  const acceptBtn2 = page2.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn2).toBeVisible({ timeout: 5000 });

  // 5. Peer 2 waits 6 seconds (simulating human reaction time) and accepts the call
  await page2.waitForTimeout(6000);
  await acceptBtn2.click({ force: true });

  // Both peers should transition to "Audio Call Active"
  await expect(page1.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  await expect(page2.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  console.log('Audio call successfully established between Peer 1 and Peer 2!');

  await page1.waitForTimeout(1000);

  const pc1Details = await page1.evaluate(async () => {
    const pc = window.__pcs[0];
    const senders = pc ? pc.getSenders().map(s => ({
      kind: s.track?.kind,
      enabled: s.track?.enabled,
      readyState: s.track?.readyState,
      muted: s.track?.muted,
    })) : [];
    const receivers = pc ? pc.getReceivers().map(r => ({
      kind: r.track?.kind,
      enabled: r.track?.enabled,
      readyState: r.track?.readyState,
      muted: r.track?.muted,
    })) : [];
    const remoteAudio = document.getElementById('remote-audio');
    const remoteAudioDetails = remoteAudio ? {
      hasSrcObject: !!remoteAudio.srcObject,
      paused: remoteAudio.paused,
      muted: remoteAudio.muted,
      volume: remoteAudio.volume,
      isConnected: remoteAudio.isConnected,
    } : null;

    let bytesSent = 0;
    let bytesReceived = 0;
    if (pc) {
      const stats = await pc.getStats();
      stats.forEach(report => {
        if (report.type === 'outbound-rtp' && report.kind === 'audio') {
          bytesSent = report.bytesSent;
        }
        if (report.type === 'inbound-rtp' && report.kind === 'audio') {
          bytesReceived = report.bytesReceived;
        }
      });
    }

    return { senders, receivers, remoteAudio: remoteAudioDetails, bytesSent, bytesReceived, signalingState: pc?.signalingState, connectionState: pc?.connectionState };
  });

  const pc2Details = await page2.evaluate(async () => {
    const pc = window.__pcs[0];
    const senders = pc ? pc.getSenders().map(s => ({
      kind: s.track?.kind,
      enabled: s.track?.enabled,
      readyState: s.track?.readyState,
      muted: s.track?.muted,
    })) : [];
    const receivers = pc ? pc.getReceivers().map(r => ({
      kind: r.track?.kind,
      enabled: r.track?.enabled,
      readyState: r.track?.readyState,
      muted: r.track?.muted,
    })) : [];
    const remoteAudio = document.getElementById('remote-audio');
    const remoteAudioDetails = remoteAudio ? {
      hasSrcObject: !!remoteAudio.srcObject,
      paused: remoteAudio.paused,
      muted: remoteAudio.muted,
      volume: remoteAudio.volume,
      isConnected: remoteAudio.isConnected,
    } : null;

    let bytesSent = 0;
    let bytesReceived = 0;
    if (pc) {
      const stats = await pc.getStats();
      stats.forEach(report => {
        if (report.type === 'outbound-rtp' && report.kind === 'audio') {
          bytesSent = report.bytesSent;
        }
        if (report.type === 'inbound-rtp' && report.kind === 'audio') {
          bytesReceived = report.bytesReceived;
        }
      });
    }

    return { senders, receivers, remoteAudio: remoteAudioDetails, bytesSent, bytesReceived, signalingState: pc?.signalingState, connectionState: pc?.connectionState };
  });

  expect(pc1Details.remoteAudio).not.toBeNull();
  expect(pc1Details.remoteAudio.hasSrcObject).toBe(true);
  expect(pc1Details.remoteAudio.isConnected).toBe(true);
  expect(pc1Details.remoteAudio.paused).toBe(false);

  expect(pc2Details.remoteAudio).not.toBeNull();
  expect(pc2Details.remoteAudio.hasSrcObject).toBe(true);
  expect(pc2Details.remoteAudio.isConnected).toBe(true);
  expect(pc2Details.remoteAudio.paused).toBe(false);

  // 6. Test microphone mute toggle on Peer 1
  const muteBtn1 = page1.locator('button:has-text("Mute Mic")');
  await expect(muteBtn1).toBeVisible();
  await muteBtn1.click();
  await expect(page1.locator('button:has-text("Unmute Mic")')).toBeVisible();

  // Unmute again
  await page1.locator('button:has-text("Unmute Mic")').click();
  await expect(page1.locator('button:has-text("Mute Mic")')).toBeVisible();
  console.log('Microphone mute/unmute verified.');

  // 6b. Peer 1 mutes incoming audio (local only, peer keeps sending)
  const speakerBtn1 = page1.locator('#speaker-mute-btn');
  await expect(speakerBtn1).toContainText('Mute Speaker');
  await speakerBtn1.click();
  await expect(speakerBtn1).toContainText('Unmute Speaker');
  expect(await page1.evaluate(() => document.getElementById('remote-audio').muted)).toBe(true);
  expect(await page2.evaluate(() => document.getElementById('remote-audio').muted)).toBe(false);
  console.log('Speaker mute verified; left muted to check reset on call end.');

  // 7. Peer 2 ends the call
  const endBtn2 = page2.locator('.call-bar button:has-text("End Call")');
  await expect(endBtn2).toBeVisible();
  await endBtn2.click();

  // Both peers should return to idle call state
  await expect(page1.locator('button:has-text("📞 Audio")')).toBeVisible({ timeout: 5000 });
  await expect(page2.locator('button:has-text("📞 Audio")')).toBeVisible({ timeout: 5000 });
  await expect(page1.locator('.call-bar')).toHaveCount(0);
  expect(await page1.evaluate(() => document.getElementById('remote-audio').muted)).toBe(false);
  console.log('Audio call ended and cleaned up cleanly.');

  // Now start a second call: Peer 1 calls Peer 2 again
  console.log('--- Starting second audio call: Peer 1 calls Peer 2 again ---');
  const callBtn1Again = page1.locator('button:has-text("📞 Audio")');
  await expect(callBtn1Again).toBeVisible({ timeout: 5000 });
  await callBtn1Again.click();

  // Peer 1 should see "Calling Peer with Audio"
  await expect(page1.locator('.call-bar')).toContainText('Calling Peer with Audio', { timeout: 5000 });

  // Peer 2 should see incoming call prompt with Accept button
  const acceptBtn2Again = page2.locator('.incoming-call-box button:has-text("Accept")');
  await expect(acceptBtn2Again).toBeVisible({ timeout: 5000 });

  // Peer 2 accepts the call
  await acceptBtn2Again.click({ force: true });

  // Both peers should transition to "Audio Call Active"
  await expect(page1.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });
  await expect(page2.locator('.call-bar')).toContainText('Audio Call Active', { timeout: 10000 });

  await page1.waitForTimeout(1000);

  const pc1Details2 = await page1.evaluate(async () => {
    const pc = window.__pcs[0];
    const senders = pc ? pc.getSenders().map(s => ({
      kind: s.track?.kind,
      enabled: s.track?.enabled,
      readyState: s.track?.readyState,
      muted: s.track?.muted,
    })) : [];
    const receivers = pc ? pc.getReceivers().map(r => ({
      kind: r.track?.kind,
      enabled: r.track?.enabled,
      readyState: r.track?.readyState,
      muted: r.track?.muted,
    })) : [];
    let bytesReceived = 0;
    let bytesSent = 0;
    if (pc) {
      const stats = await pc.getStats();
      stats.forEach(report => {
        if (report.type === 'inbound-rtp' && report.kind === 'audio') {
          bytesReceived = report.bytesReceived;
        }
        if (report.type === 'outbound-rtp' && report.kind === 'audio') {
          bytesSent = report.bytesSent;
        }
      });
    }
    return { signalingState: pc?.signalingState, connectionState: pc?.connectionState, senders, receivers, bytesSent, bytesReceived };
  });
  expect(pc1Details2.connectionState).toBe('connected');
  expect(pc1Details2.bytesReceived).toBeGreaterThan(0);
  await expect(page1.locator('#speaker-mute-btn')).toContainText('Mute Speaker');
  await expect(page1.locator('#speaker-mute-btn')).not.toContainText('Unmute');

  await context1.close();
  await context2.close();
});
