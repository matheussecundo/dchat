import { chromium } from '@playwright/test';
import path from 'path';
import fs from 'fs';

const targetDir = process.argv[2] || path.resolve(process.cwd(), '../screenshots');
fs.mkdirSync(targetDir, { recursive: true });

const browser = await chromium.launch();

try {
  // 1. Desktop viewport (1280x720)
  const desktopPage = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  await desktopPage.goto('http://localhost:8443');
  await desktopPage.waitForSelector('text=🔒 dchat');
  await desktopPage.screenshot({ path: path.join(targetDir, 'desktop.png') });

  // 2. Mobile viewport (390x844 - iPhone 14/15 size)
  const mobilePage = await browser.newPage({ viewport: { width: 390, height: 844 }, isMobile: true });
  await mobilePage.goto('http://localhost:8443');
  await mobilePage.waitForSelector('text=🔒 dchat');
  await mobilePage.screenshot({ path: path.join(targetDir, 'mobile.png') });

  console.log(`Screenshots successfully captured in: ${targetDir}`);
} finally {
  await browser.close();
}
