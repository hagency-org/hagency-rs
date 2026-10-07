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
  await page.goto(`${config.base}/console/setup/`);
  await page.locator('.setup-progress button').nth(1).click();
  await page.getByRole('button', { name: 'New server engagement', exact: true }).click();
  const form = page.locator('[data-association-form]');
  const submissions = [];
  let loseReply = true;
  await context.route('**/console/api/palpo/associations', async route => {
    if (route.request().method() !== 'POST') { await route.continue(); return; }
    submissions.push(route.request().postDataJSON());
    if (loseReply) {
      loseReply = false;
      const headers = await route.request().allHeaders();
      assert(headers.cookie, 'the browser supplied its authenticated session');
      const response = await route.fetch({ headers: { ...headers, origin: config.base, 'sec-fetch-site': 'same-origin' }, maxRedirects: 0, maxRetries: 0 });
      assert.equal(response.status(), 200);
      await route.abort();
    } else { await route.continue(); }
  });
  async function capture(name) {
    for (const width of [430, 1000]) {
      await page.setViewportSize({ width, height: 1000 });
      for (const theme of ['Light', 'Dark']) {
        const toggle = page.getByRole('button', { name: theme, exact: true });
        if (!await toggle.isVisible()) await page.locator('.rail-menu-btn').click();
        await toggle.click();
        const menu = page.locator('.rail-menu-btn');
        if (await menu.isVisible() && await menu.getAttribute('aria-expanded') === 'true') await menu.click();
        await page.evaluate(() => document.fonts.ready);
        await page.waitForFunction(() => !document.getAnimations().some(animation => animation.playState === 'running' && animation.effect?.getTiming().iterations !== Infinity));
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'no horizontal overflow');
        await page.screenshot({ path: join(config.output, `${name}-${width}-${theme}.png`), fullPage: true });
      }
    }
    await page.getByRole('button', { name: 'Light', exact: true }).click();
  }
  await form.locator('[name=homeserver]').fill(config.palpo);
  await form.locator('[name=name]').fill('My desktop connection');
  await capture('server');
  await form.getByRole('button', { name: 'Continue', exact: true }).click();
  await form.locator('[name=ownerMxid]').fill('@owner:example.test');
  await form.locator('[name=coordinatorMxid]').fill('@coordinator:example.test');
  assert.equal(await form.locator('input[type=password],input[type=file]').count(), 0);
  await capture('people');
  await form.getByRole('button', { name: 'Continue', exact: true }).click();
  assert.equal(submissions.length, 0, 'review sends no association command');
  await capture('review');
  await page.getByRole('button', { name: '中文', exact: true }).click();
  await form.getByRole('heading', { name: '核对连接申请', exact: true }).waitFor();
  assert.match(await form.innerText(), /My desktop connection/);
  await page.reload();
  await form.getByRole('heading', { name: '核对连接申请', exact: true }).waitFor();
  assert.match(await form.innerText(), /@owner:example.test/);
  await page.screenshot({ path: join(config.output, 'review-zh.png'), fullPage: true });
  await page.getByRole('button', { name: 'English', exact: true }).click();
  await form.getByRole('button', { name: 'Request connection', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'Could not confirm the request' }).waitFor();
  await page.reload();
  await form.getByRole('button', { name: 'Request connection', exact: true }).click();
  await form.waitFor({ state: 'hidden' });
  assert.equal(submissions.length, 2);
  assert.deepEqual(submissions[0], submissions[1], 'retry after reload preserves exact intent and deadline');
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
  await page.getByRole('button', { name: 'Continue to resources', exact: true }).waitFor();
  assert.equal(await page.getByRole('button', { name: 'Continue to resources', exact: true }).isEnabled(), true);
  await page.screenshot({ path: join(config.output, 'connected.png'), fullPage: true });
  await page.getByRole('button', { name: '中文', exact: true }).click();
  await page.getByRole('heading', { name: '建立服务器关联', exact: true }).waitFor();
  await page.locator('[data-association-phase=connected]').getByText('连接已验证', { exact: true }).waitFor();
  await page.screenshot({ path: join(config.output, 'connected-zh.png'), fullPage: true });
  await page.getByRole('button', { name: '继续配置资源', exact: true }).click();
  await page.getByRole('heading', { name: '3. 选择共享的资源', exact: true }).waitFor();
  assert.equal(await page.locator('input[type=file]').count(), 0);
  assert.equal(await page.getByRole('button', { name: 'Offer resource', exact: true }).count(), 0);
  const allocation = page.getByRole('link', { name: '配置服务器资源分配', exact: true });
  try {
    assert.equal(await allocation.getAttribute('href', { timeout: 8000 }), `/console/resources/new/?server_engagement_id=${id}`);
    await allocation.click({ timeout: 8000 });
    await page.locator('#configuration-engagement').waitFor({ timeout: 8000 });
    await page.waitForFunction(expected => document.querySelector('#configuration-engagement')?.value === expected, id, { timeout: 8000 });
  } catch (error) {
    await writeFile(join(config.output, 'allocation-failure.txt'), await page.locator('body').innerText({ timeout: 1000 }));
    await page.screenshot({ path: join(config.output, 'allocation-failure.png'), fullPage: true });
    throw error;
  }
  assert.equal(await page.locator('#configuration-engagement').inputValue(), id, 'verified connection carries into allocation form');
  assert.deepEqual(failures, []);
  await writeFile(join(config.output, 'report.json'), JSON.stringify({ passed: true,
    scope: 'Real native console and importer with an isolated Palpo approval/probe fixture; Palpo authority is separately tested in palpo-operations',
    checks: ['starts from the desktop UI without user tokens or files', 'Rinx action link contains no capability', 'reload retains one request', 'owner and admin progress are distinct', 'native backend imports credentials without exposing them', 'import alone does not display connected', 'verified connection leads to the unified resource configuration entry', 'setup has one allocation entry', 'connection draft and step survive language change and reload', 'lost reply retries the same intent after reload', 'review sends no command', 'verified connection is preselected in the existing resource allocation form', '430/1000 light and dark screenshots have no horizontal overflow'] }, null, 2));
  await context.close();
} finally { await browser.close(); }
