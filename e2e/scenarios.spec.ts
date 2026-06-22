import { expect, test, type Page } from '@playwright/test';
import { mkdir } from 'node:fs/promises';

const screenshotDir = 'e2e/__screenshots__';
const screenshotPath = (scenario: string) => `${screenshotDir}/${scenario}.png`;

async function captureScenario(page: Page, scenario: string) {
  await mkdir(screenshotDir, { recursive: true });
  await page.screenshot({ path: screenshotPath(scenario), fullPage: true });
}

test('default scenario shows the main import workspace', async ({ page }) => {
  await page.goto('/?scenario=default');

  await expect(page.getByText('Immich Shuttle').first()).toBeVisible();
  await expect(page.getByText('Source').first()).toBeVisible();
  await expect(page.getByText('Albums').first()).toBeVisible();
  await expect(page.getByText('Import queue').first()).toBeVisible();
  await expect(page.getByText(/photos/i).first()).toBeVisible();

  await captureScenario(page, 'default');
});

test('onboarding scenario shows first-run connection fields', async ({ page }) => {
  await page.goto('/?scenario=onboarding');

  await expect(page.getByText('Welcome to Immich Shuttle').first()).toBeVisible();
  await expect(page.getByText('Primary Immich URL').first()).toBeVisible();

  await captureScenario(page, 'onboarding');
});

test('importing scenario shows running queue progress', async ({ page }) => {
  await page.goto('/?scenario=importing');

  await expect(page.getByText('Import queue').first()).toBeVisible();
  await expect(page.getByText(/Running|Uploaded/).first()).toBeVisible();
  await expect(page.locator('[data-slot="progress"]').first()).toBeVisible();

  await captureScenario(page, 'importing');
});

test('wipe scenario shows pending wipe confirmation', async ({ page }) => {
  await page.goto('/?scenario=wipe');

  await expect(page.getByText(/Awaiting wipe confirmation/).first()).toBeVisible();

  await captureScenario(page, 'wipe');
});

test('empty scenario shows source and queue empty states', async ({ page }) => {
  await page.goto('/?scenario=empty');

  await expect(page.getByText('Drag photos or a folder here').first()).toBeVisible();
  await expect(page.getByText('No imports yet').first()).toBeVisible();

  await captureScenario(page, 'empty');
});

test('history tab lists past imports', async ({ page }) => {
  await page.goto('/?scenario=default');

  await page.getByRole('tab', { name: /History/ }).click();
  await expect(page.getByText('Import history').first()).toBeVisible();
  await expect(page.getByText(/uploaded/).first()).toBeVisible();
  await expect(page.getByText('Clear history').first()).toBeVisible();

  await captureScenario(page, 'history');
});

test('logs dialog shows recent activity', async ({ page }) => {
  await page.goto('/?scenario=default');

  await page.getByRole('button', { name: /^Logs$/ }).click();
  await expect(page.getByText('Application logs')).toBeVisible();
  await expect(page.getByText(/import_complete/).first()).toBeVisible();

  await captureScenario(page, 'logs');
});

test('parallel uploads input validates its range', async ({ page }) => {
  await page.goto('/?scenario=default');

  const input = page.getByRole('spinbutton', { name: /Parallel uploads/ });
  await input.fill('69');
  await expect(page.getByText(/between 1 and 20/)).toBeVisible();
  await input.fill('6');
  await expect(page.getByText(/between 1 and 20/)).toHaveCount(0);

  await captureScenario(page, 'concurrency');
});

test('card insert scenario surfaces the auto-import banner', async ({ page }) => {
  await page.goto('/?scenario=cardinsert');

  await expect(page.getByText(/Card detected/).first()).toBeVisible();
  await captureScenario(page, 'cardinsert');

  const importBtn = page.getByRole('button', { name: /^Import$/ });
  await expect(importBtn).toBeVisible();

  // Declining hides the banner.
  await page.getByRole('button', { name: /Not now/ }).click();
  await expect(page.getByText(/Card detected/)).toHaveCount(0);
});

test('failed import lists which files failed and why', async ({ page }) => {
  await page.goto('/?scenario=default');

  await expect(page.getByText(/4 files failed/).first()).toBeVisible();
  await expect(page.getByText('IMG_0412.CR3').first()).toBeVisible();
  await expect(page.getByText(/Internal Server Error/).first()).toBeVisible();

  await captureScenario(page, 'file-errors');
});
