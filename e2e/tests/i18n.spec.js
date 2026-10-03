import { test, expect } from '@playwright/test';

test('UI localization, dynamic language switching, RTL for Arabic, and zero persistence', async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();

  // 1. Open app and verify default English localization and LTR direction
  await page.goto('/');
  await page.waitForSelector('text=🔒 dchat');

  // Verify language dropdown exists and defaults to English
  const langSelect = page.locator('select.lang-select');
  await expect(langSelect).toBeVisible();
  await expect(langSelect).toHaveValue('en');

  // Check English strings
  await expect(page.locator('header .btn-secondary')).toContainText('Scan QR');
  await expect(page.locator('header .btn-danger')).toContainText('Wipe Session');
  await expect(page.locator('.empty-state h3')).toContainText('Ephemeral P2P Encrypted Session');
  await expect(page.locator('.security-checklist')).toContainText('256-bit ChaCha20-Poly1305 E2EE Text');

  // Check document attributes
  const initialDir = await page.getAttribute('html', 'dir');
  const initialLang = await page.getAttribute('html', 'lang');
  expect(initialDir).toBe('ltr');
  expect(initialLang).toBe('en');

  // 2. Switch to Spanish (es)
  await langSelect.selectOption('es');
  await expect(page.locator('header .btn-secondary')).toContainText('Escanear QR');
  await expect(page.locator('header .btn-danger')).toContainText('Borrar sesión');
  await expect(page.locator('.empty-state h3')).toContainText('Sesión efímera cifrada P2P');
  await expect(page.locator('.security-checklist')).toContainText('Texto E2EE ChaCha20-Poly1305 de 256 bits');

  const esDir = await page.getAttribute('html', 'dir');
  const esLang = await page.getAttribute('html', 'lang');
  expect(esDir).toBe('ltr');
  expect(esLang).toBe('es');

  // 3. Switch to Arabic (ar) - Verify Right-to-Left (RTL) mode
  await langSelect.selectOption('ar');
  await expect(page.locator('header .btn-secondary')).toContainText('مسح رمز QR');
  await expect(page.locator('header .btn-danger')).toContainText('مسح الجلسة');
  await expect(page.locator('.empty-state h3')).toContainText('جلسة P2P مشفرة سريعة الزوال');

  const arDir = await page.getAttribute('html', 'dir');
  const arLang = await page.getAttribute('html', 'lang');
  expect(arDir).toBe('rtl');
  expect(arLang).toBe('ar');

  // 4. Switch to Chinese Simplified (zh) - Verify returns to LTR
  await langSelect.selectOption('zh');
  await expect(page.locator('header .btn-secondary')).toContainText('扫描二维码');
  await expect(page.locator('header .btn-danger')).toContainText('清除会话');
  await expect(page.locator('.empty-state h3')).toContainText('临时点对点端到端加密会话');

  const zhDir = await page.getAttribute('html', 'dir');
  const zhLang = await page.getAttribute('html', 'lang');
  expect(zhDir).toBe('ltr');
  expect(zhLang).toBe('zh');

  // 5. Verify Zero Persistence Invariant: language selection does not write to localStorage or sessionStorage
  const storage = await page.evaluate(() => ({
    local: localStorage.length,
    session: sessionStorage.length,
  }));
  expect(storage.local).toBe(0);
  expect(storage.session).toBe(0);

  await context.close();
});
