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
