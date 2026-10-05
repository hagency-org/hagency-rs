import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { chromium } from 'playwright-core';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const config = JSON.parse((await lines.next()).value);
const browser = await chromium.launch({ executablePath: process.env.HAGENCY_BROWSER_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true,
  args: ['--disable-background-networking', '--disable-component-update', '--no-default-browser-check'] });
try {
  const context = await browser.newContext({ viewport: { width: 1200, height: 1000 }, serviceWorkers: 'block' });
  const page = await context.newPage(), errors = [], submitted = [];
  page.on('pageerror', e => errors.push(e.message));
  let loseFirst = true;
  await context.route('**/*', async route => {
    const req = route.request(), url = new URL(req.url());
    if (url.origin !== config.base) { errors.push('unexpected external request'); await route.abort(); return; }
    if (req.method() === 'POST' && url.pathname.endsWith('/contributions')) {
      submitted.push(req.postDataJSON());
      if (loseFirst) {
        loseFirst = false;
        // route.fetch uses Playwright's HTTP client, which drops browser fetch
        // metadata. Preserve the checked page origin and actual session cookie.
        const headers = await req.allHeaders();
        assert(headers.cookie); assert.equal(new URL(page.url()).origin, config.base);
        const result = await route.fetch({ headers: { ...headers, origin: config.base, 'sec-fetch-site': 'same-origin' }, maxRedirects: 0, maxRetries: 0 });
        assert.equal(result.status(), 200);
        await route.abort('failed'); return;
      }
    }
    await route.continue();
  });
  await page.goto(config.url);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  await page.goto(`${config.base}/console/resources/?resource_id=${config.resource}`);
  const panel = page.locator('[data-native-contributions]');
  await panel.locator('#contribution-fleet option', { hasText: 'example.test' }).waitFor({ state: 'attached' });
  await panel.locator('#contribution-fleet').selectOption(config.fleet);
  await panel.locator('#contribution-tokens').fill('400');
  await panel.locator('#contribution-agents').fill('4');
  await panel.locator('#contribution-rate').fill('100');
  await panel.locator('#contribution-expiry').fill(new Date(Date.now() + 86400000).toISOString().slice(0, 16));
  await panel.getByRole('button', { name: 'Reserve contribution', exact: true }).click();
  await panel.getByRole('alert').waitFor();
  assert.match(await panel.getByRole('alert').innerText(), /result is not confirmed/i);
  await page.reload();
  await panel.getByText('A saved request is ready to retry', { exact: false }).waitFor();
  await panel.getByRole('button', { name: 'Retry saved request', exact: true }).click();
  await page.waitForFunction(() => Object.keys(sessionStorage).every(k => !k.startsWith('hagency:contribution:')));
  await panel.locator('[data-contribution-id]').waitFor();
  assert.equal(await panel.locator('[data-contribution-id]').count(), 1);
  assert.equal(submitted.length, 2); assert.deepEqual(submitted[0], submitted[1]);
  assert.equal(await page.evaluate(() => Object.keys(sessionStorage).filter(k => k.startsWith('hagency:contribution:')).length), 0);
  assert.match(await panel.innerText(), /Reserved in Hagency/);
  const captures = process.env.HAGENCY_CONSOLE_SCREENSHOTS;
  if (captures) {
    await mkdir(captures, { recursive: true });
    await panel.screenshot({ path: join(captures, 'contribution-desktop.png') });
    // Capture the complete narrow layout in one viewport: the console owns
    // its scroll container, so fullPage would stitch repeated fixed shells.
    await page.setViewportSize({ width: 430, height: 1800 });
    await panel.screenshot({ path: join(captures, 'contribution-430.png') });
    await page.setViewportSize({ width: 430, height: 900 });
  }
  page.once('dialog', d => d.accept());
  await panel.getByRole('button', { name: 'Revoke', exact: true }).click();
  await panel.locator('[data-contribution-id]').getByText('Revoked', { exact: true }).waitFor();
  await page.reload();
  await panel.locator('[data-contribution-id]').getByText('Revoked', { exact: true }).waitFor();
  assert.equal(await panel.getByRole('button', { name: 'Revoke', exact: true }).isDisabled(), true);
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ passed: true, exactRetry: true, reload: true, contributions: 1, revoked: true, captures: captures ?? null }));
} finally { await browser.close(); }
