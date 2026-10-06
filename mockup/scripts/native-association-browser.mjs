import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { chromium } from 'playwright-core';
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const config = JSON.parse((await lines.next()).value);
const browser = await chromium.launch({ executablePath: process.env.HAGENCY_BROWSER_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
await mkdir(config.output, { recursive: true });
try {
  const context = await browser.newContext({ serviceWorkers: 'block', viewport: { width: 1280, height: 1000 } });
  const page = await context.newPage(); const failures = [];
  page.on('pageerror', e => failures.push(e.message));
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
    else await route.continue();
  });
  await page.goto(config.url);
  await page.locator('[data-native-state="ready"][aria-busy="false"]').waitFor();
  await page.goto(`${config.base}/console/server-engagements/`);
  await page.getByRole('button', { name: 'New server engagement', exact: true }).click();
  const form = page.locator('[data-association-form]');
  await form.locator('[name=homeserver]').fill(config.palpo);
  await form.locator('[name=ownerMxid]').fill('@owner:example.test');
  await form.locator('[name=coordinatorMxid]').fill('@coordinator:example.test');
  await form.locator('[name=name]').fill('My desktop connection');
  assert.equal(await form.locator('input[type=password],input[type=file]').count(), 0);
  await page.screenshot({ path: join(config.output, 'new-server-engagement.png'), fullPage: true });
  await form.getByRole('button', { name: 'Request connection', exact: true }).click();
  const row = page.locator('[data-association-phase=awaiting_owner]'); await row.waitFor();
  assert.match(await row.getByRole('link', { name: 'Open request in Rinx' }).getAttribute('href'), /^rinx:\/\/palpo\/action\/action_[a-f0-9]{32}$/);
  assert.equal(await page.getByRole('button', { name: 'Allocate resource', exact: true }).count(), 0);
  // Reloading must observe the same durable request without another POST.
  const id = await row.getAttribute('data-association-id');
  await page.reload(); await row.waitFor();
  assert.equal(await row.getAttribute('data-association-id'), id);
  await page.screenshot({ path: join(config.output, 'owner-confirmation.png'), fullPage: true });
  process.stdout.write('owner_seen\n'); await lines.next();
  await page.locator('[data-association-phase=awaiting_admin]').waitFor();
  process.stdout.write('admin_seen\n'); await lines.next();
  await page.locator('[data-association-phase=awaiting_connection]').waitFor();
  assert.match(await page.locator('[data-native-associations]').innerText(), /Verify connection/);
  const publicRows = await page.evaluate(async () => (await fetch('/console/api/palpo/associations')).json());
  assert.equal(publicRows.associations[0].imported, true);
  assert.doesNotMatch(JSON.stringify(publicRows), /pairing-test-|as_token|hs_token|secret|"token"|"profile"/);
  process.stdout.write('imported_seen\n'); await lines.next();
  await page.locator('[data-association-phase=connected]').waitFor();
  assert.equal(await page.locator('[data-association-phase=connected]').getByRole('link', { name: 'New resource configuration', exact: true }).getAttribute('href'), '/console/resources/new/');
  await page.screenshot({ path: join(config.output, 'connected.png'), fullPage: true });
  await page.getByRole('button', { name: '中文', exact: true }).click();
  await page.getByRole('heading', { name: '建立服务器关联', exact: true }).waitFor();
  await page.getByText('连接已验证', { exact: true }).waitFor();
  await page.screenshot({ path: join(config.output, 'connected-zh.png'), fullPage: true });
  await page.goto(`${config.base}/console/setup/`);
  assert.equal(await page.locator('input[type=file]').count(), 0);
  assert.equal(await page.getByRole('button', { name: 'Offer resource', exact: true }).count(), 0);
  assert.deepEqual(failures, []);
  await writeFile(join(config.output, 'report.json'), JSON.stringify({ passed: true,
    scope: 'Real native console and importer with an isolated Palpo approval/probe fixture; Palpo authority is separately tested in palpo-operations',
    checks: ['starts from the desktop UI without user tokens or files', 'Rinx action link contains no capability', 'reload retains one request', 'owner and admin progress are distinct', 'native backend imports credentials without exposing them', 'import alone does not display connected', 'verified connection leads to the unified resource configuration entry', 'setup has no separate allocation form'] }, null, 2));
  await context.close();
} finally { await browser.close(); }
