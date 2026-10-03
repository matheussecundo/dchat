import { test, expect } from '@playwright/test';

// The 1:1 call/file flow is replaced by the group model; rewritten in milestone 8c.
test.skip(true, 'Rewritten for group rooms in milestone 8c');

test('2-peer ephemeral WebRTC P2P encrypted file sharing with multi-chunk transfer', async ({ browser }) => {
  const context1 = await browser.newContext({ acceptDownloads: true });
  const context2 = await browser.newContext({ acceptDownloads: true });

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  page1.on('console', msg => {
    if (msg.type() === 'error') console.log('Page1 ERROR:', msg.text());
  });
  page2.on('console', msg => {
    if (msg.type() === 'error') console.log('Page2 ERROR:', msg.text());
  });

  // 1. Peer 1 opens dchat
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();

  // 2. Peer 2 joins using the same secret hash
  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  // 3. Wait for P2P connection
  console.log('Waiting for P2P connection...');
  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  console.log('P2P connected!');

  // 4. Test Staging Chip: Peer 1 stages a file and removes it
  await page1.setInputFiles('#file-input-hidden', {
    name: 'temporary_draft.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('Will be removed before sending'),
  });
  await expect(page1.locator('.attachment-chip')).toBeVisible();
  await expect(page1.locator('.attachment-chip')).toContainText('temporary_draft.txt');
  // Click remove button ✕ on chip
  await page1.locator('.attachment-chip button.btn-remove-attachment').click();
  await expect(page1.locator('.attachment-chip')).toHaveCount(0);

  // 5. Peer 1 selects a multi-chunk file (150 KB > 64 KB CHUNK_SIZE => 3 chunks)
  const fileName = 'confidential_report.pdf';
  const fileContent = 'Zero-Knowledge Confidential Report Header\n' + 'A'.repeat(150000) + '\nReport Footer';
  const fileBuffer = Buffer.from(fileContent);

  await page1.setInputFiles('#file-input-hidden', {
    name: fileName,
    mimeType: 'application/pdf',
    buffer: fileBuffer,
  });
  await expect(page1.locator('.attachment-chip')).toBeVisible();
  await expect(page1.locator('.attachment-chip')).toContainText(fileName);

  // Peer 1 adds a caption and clicks Send
  const caption = 'Here is the confidential audit document for your review.';
  const input1 = page1.locator('footer.input-bar input');
  await input1.fill(caption);
  await page1.locator('footer.input-bar button:has-text("Send")').click();

  // Staging chip should be cleared after sending
  await expect(page1.locator('.attachment-chip')).toHaveCount(0);

  // 6. Verify file card appears on Peer 1 (sender)
  const p1Card = page1.locator('.file-card');
  await expect(p1Card).toBeVisible();
  await expect(p1Card).toContainText(fileName);
  await expect(p1Card).toContainText('146.5 KB');
  await expect(page1.locator('.chat-container')).toContainText(caption);

  // 7. Verify file card appears on Peer 2 (receiver)
  const p2Card = page2.locator('.file-card');
  await expect(p2Card).toBeVisible({ timeout: 5000 });
  await expect(p2Card).toContainText(fileName);
  await expect(p2Card).toContainText('146.5 KB');
  await expect(page2.locator('.chat-container')).toContainText(caption);
  const downloadBtn = p2Card.locator('button.file-download-btn');
  await expect(downloadBtn).toBeVisible();
  await expect(downloadBtn).toContainText('Download');

  // 8. Trigger download fallback in page2 to capture via Playwright's download event
  await page2.evaluate(() => {
    // Delete showSaveFilePicker so browser uses in-memory Blob + anchor download fallback
    delete window.showSaveFilePicker;
  });

  const downloadPromise = page2.waitForEvent('download');
  console.log('Peer 2 clicking Download button...');
  await downloadBtn.click();

  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe(fileName);

  // Read downloaded file stream and assert exact byte-for-byte fidelity
  const stream = await download.createReadStream();
  const chunks = [];
  for await (const chunk of stream) {
    chunks.push(chunk);
  }
  const downloadedText = Buffer.concat(chunks).toString('utf-8');
  expect(downloadedText).toBe(fileContent);
  console.log('File successfully transferred, decrypted, and verified bit-for-bit!');

  // 9. Verify UI completed status on both sides
  await expect(p2Card).toContainText('Download complete', { timeout: 10000 });
  await expect(p1Card).toContainText('Sent successfully', { timeout: 10000 });

  // 10. Verify Zero Persistence Invariant
  const p1Storage = await page1.evaluate(() => ({
    local: localStorage.length,
    session: sessionStorage.length,
  }));
  const p2Storage = await page2.evaluate(() => ({
    local: localStorage.length,
    session: sessionStorage.length,
  }));
  expect(p1Storage.local).toBe(0);
  expect(p1Storage.session).toBe(0);
  expect(p2Storage.local).toBe(0);
  expect(p2Storage.session).toBe(0);
  console.log('Zero persistence confirmed: no stored messages or transfers.');

  // 11. Verify Reload Memory Wipe
  await page2.reload();
  await page2.waitForSelector('text=🔒 dchat');
  await expect(page2.locator('.file-card')).toHaveCount(0);
  await expect(page2.locator('.message-bubble')).toHaveCount(0);
  console.log('Memory wipe confirmed: chat and transfers wiped on reload.');

  await context1.close();
  await context2.close();
});

test('File offer decline by receiver cancels transfer on both peers', async ({ browser }) => {
  const context1 = await browser.newContext();
  const context2 = await browser.newContext();

  const page1 = await context1.newPage();
  const page2 = await context2.newPage();

  // 1. Setup session
  await page1.goto('/');
  await page1.waitForSelector('text=🔒 dchat');
  await page1.waitForFunction(() => window.location.hash.includes('#room=') && window.location.hash.includes('&key='));
  const peer1Url = page1.url();

  await page2.goto(peer1Url);
  await page2.waitForSelector('text=🔒 dchat');

  await expect(page1.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });
  await expect(page2.locator('.status-indicator')).toContainText('Connected (E2EE P2P Active)', { timeout: 15000 });

  // 2. Peer 1 sends a file offer
  await page1.setInputFiles('#file-input-hidden', {
    name: 'declined_file.bin',
    mimeType: 'application/octet-stream',
    buffer: Buffer.from('Some sensitive bytes'),
  });
  await page1.locator('footer.input-bar button:has-text("Send")').click();

  // 3. Peer 2 receives offer and clicks Decline
  const p2Card = page2.locator('.file-card');
  await expect(p2Card).toBeVisible({ timeout: 5000 });
  const declineBtn = p2Card.locator('button:has-text("Decline")');
  await expect(declineBtn).toBeVisible();
  await declineBtn.click();

  // 4. Verify cancelled status on Peer 2 and Peer 1
  await expect(p2Card).toContainText('Cancelled', { timeout: 5000 });
  const p1Card = page1.locator('.file-card');
  await expect(p1Card).toContainText('Cancelled', { timeout: 5000 });
  console.log('File offer decline correctly propagated to both peers!');

  await context1.close();
  await context2.close();
});

