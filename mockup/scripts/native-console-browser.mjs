/* Real Chromium over a fresh native fixture. It receives no operator token. */
import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { chromium } from 'playwright-core';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const config = JSON.parse((await lines.next()).value);
async function fixture(command) { console.log(command); return JSON.parse((await lines.next()).value); }

/* The agent roster walk (ADR-126), shared by the full console walk and the
 * roster-only lane: the page renders the seven-column projection, the
 * SERVER-OWNED unavailable list verbatim, the null-not-zero activity arms,
 * and no lifecycle control — and no private value reaches the screen. */
async function rosterWalk(page) {
  await page.goto(`${config.base}/console/agents/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  assert((await page.locator('tbody tr').count()) >= 3, 'one roster row per seeded engagement');
  const text = await page.locator('main').innerText();
  assert.match(text, /UsageWorker/);
  // #43 item 4 + #31: the heading names what the page is and what stopping
  // needs; one login grants the controls (no separate lifecycle link).
  assert.match(text, /The agents you have lent, and what each one is doing now|你借出的 Agent，以及每个 Agent 当前的状态/);
  // #60 widened the roster to the full projection: the never-answerable
  // columns (tmux target, workspace path) are GONE from the server's gap
  // list — it is empty now — and the five real columns render by name
  // (bilingual: the executable lane walks the same page in Chinese).
  assert.match(text, /Liveness|运行状态/);
  assert.match(text, /Consumed|已消耗/);
  assert.match(text, /Last dispatch activity|最近派发活动/);
  assert.match(text, /Online|在线/);
  assert.match(text, /Last seen|最近在线/);
  assert(!/tmux|workspace_path/.test(text), 'the never-answerable columns are gone (#60)');
  // The null-not-zero arms: the active engagement carries its dispatch
  // clock; a pending one renders the unknown word — never a zero.
  // Column 7 is the activity cell in both shapes of the roster (the
  // lifecycle controls column is the 8th, only when the login carries it).
  const cells = await page.locator('tbody tr td:nth-child(9)').allInnerTexts();
  assert(cells.some((c) => /^\d{4}-\d{2}-\d{2}T/.test(c)), 'the active engagement carries its dispatch clock');
  assert(cells.some((c) => c === 'Unknown' || c === '未知'), 'an engagement with no attempt row renders unknown');
  assert(cells.every((c) => c !== '0'), 'unknown is never rendered as zero');
  assert(!/private_|\/Users\/|tmux attach/.test(text), 'no private path, home or target renders');
  // One login (TS parity): the roster renders the implemented lifecycle
  // controls per row. Stop and review for every serving row; #106 added the
  // way BACK — Start — but it renders ONLY for a stopped row, and no fixture
  // row is stopped here, so it stays absent (the same predicate the
  // stop-then-start lane exercises positively).
  assert((await page.locator('[data-lifecycle-action="stop"]').count()) >= 3, 'one login renders the stop control per row');
  assert((await page.locator('[data-lifecycle-action="review"]').count()) >= 3, 'the review control renders per row');
  assert((await page.locator('[data-lifecycle-action="start"]').count()) === 0, 'no serving row advertises Start (it is the stopped row control)');
  assert((await page.locator('[data-lifecycle-action="preset"]').count()) === 0, 'the unavailable preset transition is never advertised');
}

/* The project-sides walk (ADR-132): the page renders the six-key side
 * cards with the SERVER-OWNED unavailable list verbatim, and no
 * credential value can appear on screen — the validator refuses any key
 * set other than the declared one and no declared key is a credential. */
async function projectSidesWalk(page) {
  await page.goto(`${config.base}/console/project-sides/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  const text = await page.locator('main').innerText();
  assert.match(text, /example\.test/, 'the side card renders, keyed by server name');
  assert.match(text, /Projects connected to this Hagency, and how each one is registered|连接到此 Hagency 的项目，以及各自的注册方式/);
  assert.match(text, /!reception:example\.test/, 'the reception room id renders as ordinary data');
  assert.match(text, /project_one/, 'the joined project renders');
  assert.match(text, /!project:example\.test/, 'the project room id renders');
  // The server's own gap list, verbatim: credential_kind and owner are
  // NAMED as unknown rather than invented. The list is diagnostics, so it
  // sits in the page's Technical details disclosure, one click away.
  const gaps = await page.locator('main .technical-details').first().textContent();
  assert.match(gaps, /credential_kind/);
  assert.match(gaps, /owner/);
  assert(!/as_token|hs_token|asToken|hsToken/.test(text), 'no credential word on screen');
  assert(!/@owner:example\.test/.test(text), 'the owner mxid stays withheld');
  assert(!/!private:example\.test/.test(text), 'the owner DM room stays withheld');
  // #45 parity row #33 joined this page: register-a-side and
  // generate-registration render beside the read-only observation.
  // #51 added the connection probe: register, generate, test connection
  // and refresh — four controls now. The Palpo import's Connect makes five;
  // it stays disabled until a downloaded configuration is picked.
  assert((await page.locator('main button').count()) === 5, 'connect, register, generate, test connection and refresh are the controls');
  const connect = page.locator('[data-palpo-import] button');
  assert(await connect.isDisabled(), 'Connect waits for a picked configuration');
  assert.match(await page.locator('[data-palpo-import]').innerText(), /Download Hagency configuration/);
}

/* The tasks page's WRITE journey (board #107), shared by the tasks-only lane:
 * create a task, comment on it and SEE the comment come back, take the move
 * the server itself offers, then delete it — every step through the page's OWN
 * controls. A page that renders perfectly can still refuse the action, and the
 * failure mode this catches is a click that sends NOTHING at all: the retired
 * regression threw inside the click handler before any request left the
 * browser, so no toast and no row change ever appeared. An assertion on the
 * rendered list alone cannot see that; waiting for the SERVED reply can.
 * The status words (created/accepted/…) are wire values, intentionally not
 * translated, so only the field LABELS move between locales. */
async function tasksWalk(page) {
  const zh = config.executable === true;
  await page.goto(`${config.base}/console/tasks/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  const title = `served walk ${Date.now()}`;
  await page.getByRole('button', { name: zh ? '新建任务' : 'New task', exact: true }).click();
  await page.locator('label', { hasText: zh ? '标题' : 'Title' }).locator('input').fill(title);
  await page.getByRole('button', { name: zh ? '创建' : 'Create', exact: true }).click();
  const row = page.locator('tbody tr', { hasText: title });
  await row.first().waitFor({ timeout: 20_000 });
  await row.first().click();
  await page.locator('h3', { hasText: title }).first().waitFor({ timeout: 20_000 });

  // Comment: the SERVED reply must render back as this task's own comment
  // (author included), which a click sending nothing can never produce.
  const note = `served note ${Date.now()}`;
  const box = page.getByPlaceholder(zh ? '添加评论…' : 'Add a comment…');
  await box.fill(note);
  const comment = page.getByRole('button', { name: zh ? '评论' : 'Comment', exact: true });
  assert.equal(await comment.isEnabled(), true, 'the comment control enables once text is present');
  await comment.click();
  await page.locator('dl.kv', { hasText: note }).waitFor({ timeout: 20_000 });
  assert.match(await page.locator('dl.kv').last().innerText(), new RegExp(note), 'the comment renders back');

  // Transition: the first offered move is legal by construction (the server
  // serves each row its own `next`), so pressing it must move the SERVED state.
  const move = page.getByRole('button', { name: /^→ \S/ }).first();
  await move.waitFor({ timeout: 20_000 });
  const to = (await move.innerText()).replace(/^→\s*/, '').trim();
  const before = await page.locator('dl.kv').first().locator('dd').first().innerText();
  assert.notEqual(to, before, 'the offered move leaves the current state');
  await move.click();
  await page.locator(`button:has-text("→ ${to}")`).waitFor({ state: 'detached', timeout: 20_000 });
  assert.equal(await page.locator('dl.kv').first().locator('dd').first().innerText(), to, 'the served state moved');

  // Delete: the page's own confirm, then the row leaves the served list.
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: zh ? '删除' : 'Delete', exact: true }).click();
  await row.first().waitFor({ state: 'detached', timeout: 20_000 });
  return `created, commented, transitioned to ${to} and deleted ${JSON.stringify(title)}`;
}

const browser = await chromium.launch({ executablePath: process.env.HAGENCY_BROWSER_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true,
  args: ['--disable-background-networking', '--disable-component-update', '--no-default-browser-check'] });
if (config.roster) {
  // The roster-only lane (ADR-126 browser scenario): same read-only ticket
  // the usage walk exchanges, one page, no operator token in the browser.
  try {
    const context = await browser.newContext({ serviceWorkers: 'block' });
    const page = await context.newPage();
    const failures = []; const urls = [];
    page.on('pageerror', (error) => failures.push(error.message));
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url()); urls.push(url.toString());
      if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
      else await route.continue();
    });
    await page.goto(config.url);
    await page.locator('[data-native-state="ready"]').first().waitFor();
    await rosterWalk(page);
    assert(urls.every((url) => !url.includes('access=')), 'no ticket value in a request URL');
    assert(!/private_|operator\.token/.test(await page.locator('main').innerText()), 'no credential value on screen');
    assert.deepEqual(failures, []);
    console.log('PASS native agent roster browser');
  } finally { await browser.close(); }
  process.exit(0);
}
if (config.lifecycle) {
  // The lifecycle lane (ADR-130 browser scenario): ONE access link (the
  // harness mints it and hands it over stdin; the browser mints nothing)
  // carries lifecycle authority — TS parity: no second ticket, no read-only
  // phase — and no external request leaves the page.
  try {
    const context = await browser.newContext({ serviceWorkers: 'block' });
    const page = await context.newPage();
    const failures = []; const urls = [];
    page.on('pageerror', (error) => failures.push(error.message));
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url()); urls.push(url.toString());
      if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
      else await route.continue();
    });
    await page.goto(config.url);
    await page.locator('[data-native-state="ready"]').first().waitFor();
    // The exchange lands on the usage page; the roster is where the
    // lifecycle controls render (one login carries the authority).
    await page.goto(`${config.base}/console/agents/`);
    await page.locator('[data-native-state="ready"]').first().waitFor();
    assert((await page.locator('[data-lifecycle-action="stop"]').count()) >= 3, 'one login renders the stop control per row');
    assert((await page.locator('[data-lifecycle-action="start"]').count()) === 0, 'no serving row advertises Start (board #106: Start is the stopped row control)');
    assert((await page.locator('[data-lifecycle-action="preset"]').count()) === 0, 'the roster does not advertise the unavailable preset transition');
    await page.locator(`[data-engagement-id="${config.engagement}"] [data-lifecycle-action="review"]`).click();
    // Board #111 acceptance, at the served binary: the review page must LIST
    // the dispatch the session quarantine is about. This fixture settles the
    // agent's own started `private_dispatch` by lease expiry, so its session
    // refuses every turn while the dispatch has NO `dispatch_stops` row — the
    // exact live trap. Before the fix this panel said "No unsettled stopped
    // task in this page." and the operator had no route out.
    await page.locator('[data-recovery-inspect="private_dispatch"]').waitFor({ timeout: 15_000 });
    await page.locator('[data-recovery-inspect="resolution_dispatch"]').click();
    // A failure here is silent in the DOM (the panel renders its error
    // word instead of the inspection), so surface what actually rendered.
    try {
      await page.locator('[data-recovery-inspection]').waitFor({ timeout: 15_000 });
    } catch (error) {
      const panel = await page.locator('[data-recovery-panel]').innerText().catch(() => '(no recovery panel)');
      throw new Error(`inspection never rendered; recovery panel says:\n${panel}\n${error.message}`);
    }
    assert.match(await page.locator('[data-recovery-inspection]').innerText(), /result\.txt/);
    assert.equal(await page.locator('[data-recovery-action]').count(), 3);
    await page.locator('[data-recovery-note]').fill('Reviewed the original offline fixture inventory. Keep this task blocked.');
    let requests = 0, frozen = null;
    await page.route('**/resolve-stopped-dispatch', async (route) => {
      const body = route.request().postData(); requests++;
      if (requests === 1) {
        frozen = body;
        const headers = await route.request().allHeaders();
        assert.equal(headers.origin, config.base);
        // Chromium adds Fetch Metadata after interception; route.fetch uses
        // Playwright's HTTP client. Preserve the browser's same-origin context
        // explicitly for this local response-loss fixture, with its own cookie.
        const response = await route.fetch({ headers: { ...headers, 'sec-fetch-site': 'same-origin' }, maxRedirects: 0, maxRetries: 0 });
        assert.equal(response.status(), 200);
        await route.abort('failed'); // Native commit exists; browser loses its ACK.
      } else {
        assert.equal(body, frozen, 'unknown decision replay is byte-identical');
        await route.continue();
      }
    });
    await page.locator('[data-recovery-action="keep_blocked"]').click();
    await page.locator('[data-recovery-retry]').waitFor();
    assert.equal(requests, 1, 'response loss does not automatically retry');
    assert(await page.locator('[data-recovery-action="continue"]').isDisabled());
    const secret = JSON.parse(frozen).inspectionToken;
    assert(!(await page.locator('main').innerText()).includes(secret));
    assert(!(await page.evaluate(() => JSON.stringify([localStorage, sessionStorage]))).includes(secret));
    await page.locator('[data-recovery-retry]').click();
    await page.locator('[data-recovery-result]').waitFor();
    assert.match(await page.locator('[data-recovery-result]').innerText(), /blocked/);
    assert.equal(requests, 2);
    assert(urls.every((url) => !url.includes('access=')), 'no ticket value in a request URL');
    // Board #111: the review page is SPECIFIED to list the quarantined
    // dispatch's id, and this fixture names it `private_dispatch`
    // (console/fixture.rs) — so that one identifier the page must render is
    // allowed. The guard still catches every OTHER fixture-internal or
    // credential value (a token, a storage path, an account/seat id).
    assert(!/private_(?!dispatch)|operator\.token/.test(await page.locator('main').innerText()), 'no credential value on screen');
    assert.deepEqual(failures, []);
    console.log('PASS native agent lifecycle browser');
  } finally { await browser.close(); }
  process.exit(0);
}
if (config.sides) {
  try {
    const context = await browser.newContext({ serviceWorkers: 'block' });
    const page = await context.newPage();
    const failures = []; const urls = [];
    page.on('pageerror', (error) => failures.push(error.message));
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url()); urls.push(url.toString());
      if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
      else await route.continue();
    });
    await page.goto(config.url);
    await page.locator('[data-native-state="ready"]').first().waitFor();
    await projectSidesWalk(page);
    assert(urls.every((url) => !url.includes('access=')), 'no ticket value in a request URL');
    assert(!/private_|operator\.token/.test(await page.locator('main').innerText()), 'no credential value on screen');
    assert.deepEqual(failures, []);
    console.log('PASS native project-sides browser');
  } finally { await browser.close(); }
  process.exit(0);
}
if (config.tasks) {
  // The tasks lane (board #107): one access link, the write journey over the
  // page's own controls, against a served binary. The link is exchanged on the
  // usage page (one login grants every console action, #31), then the walk runs.
  try {
    const context = await browser.newContext({ serviceWorkers: 'block' });
    const page = await context.newPage();
    const failures = []; const urls = [];
    page.on('pageerror', (error) => failures.push(error.message));
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url()); urls.push(url.toString());
      if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
      else await route.continue();
    });
    await page.goto(config.url);
    await page.locator('[data-native-state="ready"]').first().waitFor();
    const detail = await tasksWalk(page);
    assert(urls.every((url) => !url.includes('access=')), 'no ticket value in a request URL');
    assert(!/private_|operator\.token/.test(await page.locator('main').innerText()), 'no credential value on screen');
    assert.deepEqual(failures, []);
    console.log(`PASS native tasks browser — ${detail}`);
  } finally { await browser.close(); }
  process.exit(0);
}
try {
  const context = await browser.newContext({ serviceWorkers: 'block' });
  const page = await context.newPage();
  const failures = []; const urls = [];
  page.on('pageerror', (error) => failures.push(error.message));
  await context.route('**/*', async (route) => {
    const url = new URL(route.request().url()); urls.push(url.toString());
    if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
    else await route.continue();
  });
  await page.goto(config.url);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  assert.equal(new URL(page.url()).hash, '');
  assert.equal(await page.locator('[data-engagement-id]').getAttribute('data-engagement-id'), config.engagement);
  assert.equal(await page.locator('[data-kind="input"]').nth(0).textContent(), '4');
  assert.equal(await page.locator('[data-kind="input"]').nth(1).textContent(), '7');
  assert.match(await page.locator('main').innerText(), /Historical high-water lower bounds/);
  assert.match(await page.locator('main').innerText(), /untrusted usage evidence/);
  if (!config.executable) {
    // Delay the actual same-selection read: refresh must retain the current view.
    const path = `${config.base}/console/api/engagements/${config.engagement}/usage`;
    let release; let observed;
    const held = new Promise((resolve) => { release = resolve; });
    const started = new Promise((resolve) => { observed = resolve; });
    await context.route(path, async (route) => { observed(); await held; await route.continue(); });
    await page.evaluate(() => window.dispatchEvent(new Event('focus')));
    await started;
    assert.equal(await page.locator('[data-native-state="ready"]').first().getAttribute('aria-busy'), 'true');
    assert.equal(await page.locator('#native-engagement').count(), 1);
    assert.equal(await page.locator('[data-kind="input"]').first().textContent(), '4');
    assert.match(await page.locator('main').innerText(), /Refreshing usage/);
    const finished = page.waitForResponse(path); release(); await finished;
    await page.locator('[data-native-state="ready"][aria-busy="false"]').first().waitFor();
    await context.unroute(path);
    // A real transport failure marks the retained observation stale explicitly.
    await context.route(path, (route) => route.abort('failed'));
    await page.evaluate(() => window.dispatchEvent(new Event('focus')));
    await page.locator('[data-native-state="stale"]').waitFor();
    assert.match(await page.locator('[data-native-state="stale"] [role="alert"]').innerText(), /earlier observations may be stale/);
    assert.equal(await page.locator('[data-kind="input"]').first().textContent(), '4');
    await context.unroute(path);
    // The stale container's own Refresh (the btn-row one): the fleet panel
    // renders a second Refresh when its read fails, so the page-level
    // role query is ambiguous.
    await page.locator('[data-native-state="stale"]').getByRole('button', { name: 'Refresh', exact: true }).click();
    await page.locator('[data-native-state="ready"][aria-busy="false"]').first().waitFor();
  }
  if (process.env.HAGENCY_CONSOLE_SCREENSHOTS && !config.executable) {
    await mkdir(process.env.HAGENCY_CONSOLE_SCREENSHOTS, { recursive: true });
    await page.screenshot({ path: join(process.env.HAGENCY_CONSOLE_SCREENSHOTS, 'console-usage-en.png'), fullPage: true });
  }
  assert(!await page.locator('main').innerText().then((v) => /NaN|undefined/.test(v)));
  const cookies = await context.cookies();
  const cookie = cookies.find((c) => c.name === 'hagency_console');
  assert(cookie?.httpOnly && cookie.sameSite === 'Strict' && cookie.path === '/console');
  assert.equal(cookie.expires, -1, 'one login is a browser-session cookie — no Max-Age, no expiry to race');
  assert.equal(await page.evaluate(() => document.cookie), '');
  assert(urls.every((url) => !url.includes('access=') && !url.includes(cookie.value)));
  const rawStatus = await page.evaluate(async () => (await fetch('/api/native/v1/engagements')).status);
  assert.equal(rawStatus, 403);
  await page.getByRole('button', { name: '中文', exact: true }).click();
  await page.getByRole('button', { name: '深色', exact: true }).click();
  await page.reload();
  await page.locator('[data-native-state="ready"]').first().waitFor();
  assert.equal(await page.locator('html').getAttribute('lang'), 'zh-CN');
  assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
  assert.match(await page.locator('main').innerText(), /历史高水位下界/);
  assert.match(await page.locator('main').innerText(), /未经验证的用量证据/);
  if (process.env.HAGENCY_CONSOLE_SCREENSHOTS && !config.executable) await page.screenshot({ path: join(process.env.HAGENCY_CONSOLE_SCREENSHOTS, 'console-usage-zh.png'), fullPage: true });
  if (!config.executable) {
    console.log('CREATE_ENGAGEMENT');
    const created = JSON.parse((await lines.next()).value).engagement;
    // The usage container's own Refresh: the fleet panel renders a second
    // one when its read fails, so the page-level role query is ambiguous.
    await page.locator('[data-native-state]').first().getByRole('button', { name: '刷新', exact: true }).click();
    await page.locator(`option[value="${created}"]`).waitFor({ state: 'attached' });
    await page.locator('#native-engagement').selectOption(created);
    await page.locator(`[data-engagement-id="${created}"]`).waitFor();
    assert.equal(new URL(page.url()).searchParams.get('engagement_id'), created);
    assert.match(await page.locator('main').innerText(), /此期间没有观测。用量未知/);
    assert.equal(await page.locator('[data-kind="input"]').first().textContent(), '未知');
    await page.reload();
    await page.locator(`[data-engagement-id="${created}"]`).waitFor();
    // Hold the real B read, return to A, then release B. No response is mocked.
    await page.locator('#native-engagement').selectOption(config.engagement);
    await page.locator(`[data-engagement-id="${config.engagement}"]`).waitFor();
    let releaseRead; let observedRead;
    const heldRead = new Promise((resolve) => { releaseRead = resolve; });
    const readStarted = new Promise((resolve) => { observedRead = resolve; });
    const delayedPath = `${config.base}/console/api/engagements/${created}/usage`;
    await context.route(delayedPath, async (route) => { observedRead(); await heldRead; await route.continue(); });
    await page.locator('#native-engagement').selectOption(created);
    await readStarted;
    await page.goBack();
    await page.locator(`[data-engagement-id="${config.engagement}"]`).waitFor();
    const finishedRead = page.waitForResponse(delayedPath);
    releaseRead(); await finishedRead;
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(new URL(page.url()).searchParams.get('engagement_id'), config.engagement);
    assert.equal(await page.locator('[data-engagement-id]').getAttribute('data-engagement-id'), config.engagement);
    await context.unroute(delayedPath);
    await page.locator('#native-engagement').selectOption(created);
    await page.locator(`[data-engagement-id="${created}"]`).waitFor();
    await page.getByRole('button', { name: 'English', exact: true }).click();
    assert.equal(await page.locator('[data-kind="input"]').first().textContent(), 'Unknown');
    await page.goto(`${config.base}/console/usage/?engagement_id=does_not_exist`);
    // The engagement's own error panel: the fleet panel renders a second
    // alert when its read fails, so match the alert carrying this text.
    await page.getByRole('alert').filter({ hasText: /not present in the native service/ }).waitFor();
    assert.match(await page.locator('main').innerText(), /not present in the native service/);
    assert.equal(await page.locator('[data-kind]').count(), 0);
    await page.goto(`${config.base}/console/usage/?engagement_id=${config.engagement}`);
    await page.locator('[data-native-state="ready"]').first().waitFor();
  }
  // The alerts page — the operator close path (ADR-124 amendment). Brief 28
  // adds the read-only arm first: this lane's original link is a READ-ONLY
  // session, so the triage buttons must be ABSENT and the notice naming the
  // configuration management link rendered — the same hide rule the
  // resources page applies without `can_configure`. The scoped link then
  // walks the REAL buttons, which render only from the served `next` array.
  // Both lanes; the seed is shared. Returns to the usage page afterwards so
  // the logout assertions below run against the page they were written for.
  await page.goto(`${config.base}/console/alerts/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  assert.match(await page.locator('main').innerText(), /has drawn 100 against a ceiling of 50/);
  assert.match(await page.locator('main').innerText(), /raise the ceiling on preset private_alert_pool/);
  assert((await page.locator('tbody tr[aria-selected]').count()) >= 1, 'the seeded alert row renders and is selectable');
  // One login (TS parity): the SAME session is offered the triage controls
  // the served `next` array names — no second link, no read-only notice.
  assert((await page.locator('[data-transition]').count()) === 4, 'one login is offered the served triage controls');
  if (!config.executable) {
    // The open row offers exactly the served map: acknowledge, assign,
    // resolve, suppress (the store's own transition map).
    const buttons = page.locator('[data-transition]');
    assert(await buttons.count() === 4, 'the open row serves exactly four transitions');
    assert((await page.locator('[data-transition="acknowledged"]').count()) === 1);
    assert((await page.locator('[data-transition="assigned"]').count()) === 1);
    assert((await page.locator('[data-transition="resolved"]').count()) === 1);
    assert((await page.locator('[data-transition="suppressed"]').count()) === 1);
    // A REAL press: acknowledge, then the served map narrows to resolve/suppress.
    await page.locator('[data-transition="acknowledged"]').click();
    // The page is already in its ready state while the transition request is in
    // flight, so the wait is for the served map to change: the pressed control
    // leaves the DOM when the reply renders (bounded, so a page that never
    // re-renders still fails here rather than passing on the stale set).
    await page.locator('[data-transition="acknowledged"]').waitFor({ state: 'detached', timeout: 10_000 });
    assert((await page.locator('[data-transition="acknowledged"]').count()) === 0, 'acknowledged is no longer offered');
    assert((await buttons.count()) === 2, 'the acknowledged row serves assign and resolve');
    // To terminal: resolve, and the terminal row serves nothing.
    await page.locator('[data-transition="resolved"]').click();
    await page.locator('[data-transition="resolved"]').waitFor({ state: 'detached', timeout: 10_000 });
    assert((await buttons.count()) === 0, 'resolved is terminal');
    // The console read serves open alerts only, so the resolved row leaves the
    // list and the page shows its no-open-alerts state rather than a terminal
    // notice for a row it no longer lists.
    assert.match(await page.locator('main').innerText(), /No open alerts\. The sweep resolves them|没有未解决的告警/);
  }
  // The engagements page (the console consumer slice, read-only): the list
  // read the usage flow already carries, rendered as triage. Ready state,
  // the seeded engagement's row, and the note saying who decides what.
  await page.goto(`${config.base}/console/engagements/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  assert((await page.locator('tbody tr').count()) >= 1, 'the seeded engagement renders');
  assert.match(await page.locator('main').innerText(), /UsageWorker|NewUsageWorker/);
  assert.match(await page.locator('main').innerText(), /Agent allocations|Agent 配额/);
  assert.match(await page.locator('main').innerText(), /Coordinator approvals are reviewed in Rinx\.|协调员审批在 Rinx 中进行。/);
  // #16/#44: the verdict panel carries the operator decision — the pending
  // row renders its Approve/Reject controls. Under #31 one login carries the
  // authority to click them; this walk asserts they render for the seeded
  // pending row without clicking.
  assert((await page.locator('main button.danger').count()) >= 1, 'the pending verdict row carries its refuse control');
  // Page IN-PAGE through the seeded rows: every page reaching the ready state
  // passed validateEngagements, and the Next button disables on the null
  // cursor. The in-page pager uses the client's own page size, so the three
  // seeded rows fit one page here; the multi-page walk at ?limit=1 is pinned
  // by the Rust console test, not by this driver.
  // The executable lane runs in Chinese, like its logout step below.
  const nextButton = page.locator('button', { hasText: config.executable ? '下一页' : 'Next page' });
  let pages = 1;
  for (let i = 0; i < 6 && (await nextButton.isEnabled()); i += 1) {
    await nextButton.click();
    await page.locator('[data-native-state="ready"]').first().waitFor();
    pages += 1;
  }
  assert(await nextButton.isDisabled(), 'the cursor exhausts to null and disables Next');
  assert(pages >= 1 && pages <= 7, `walked ${pages} pages`);
  await page.locator('button', { hasText: config.executable ? '第一页' : 'First page' }).click();
  await page.locator('[data-native-state="ready"]').first().waitFor();
  // The roster page in the full walk too: the same seven-column projection
  // under the same session, bilingually asserted by rosterWalk.
  await rosterWalk(page);
  // The project-sides page likewise: the six-key side cards, the server's
  // gap list, and the credential negatives — bilingual via the walk.
  await projectSidesWalk(page);
  await page.goto(`${config.base}/console/usage/?engagement_id=${config.engagement}`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  // The fleet panel's own multi-step load (totals, sides, then one budget per
  // side) must have settled before the logout count starts, or a read sent
  // while still signed in would be counted as one sent after End access.
  await page.locator('[data-fleet-state="ready"], [data-fleet-state="error"]').first().waitFor();
  const storage = await page.evaluate(() => ({ local: Object.fromEntries(Object.entries(localStorage)), session: Object.fromEntries(Object.entries(sessionStorage)) }));
  assert.deepEqual(Object.keys(storage.local).sort(), ['hagency.locale', 'hagency.theme']);
  assert.deepEqual(storage.session, {});
  assert(!JSON.stringify(storage).includes(cookie.value));
  let releaseLogout; let observedLogout; let readsDuringLogout = 0;
  const heldLogout = new Promise((resolve) => { releaseLogout = resolve; });
  const logoutStarted = new Promise((resolve) => { observedLogout = resolve; });
  await context.route(`${config.base}/console/session`, async (route) => { observedLogout(); await heldLogout; await route.continue(); });
  const logoutReads = [];
  page.on('request', (request) => { if (request.url().includes('/console/api/')) { readsDuringLogout += 1; logoutReads.push(`${request.method()} ${request.url()}`); } });
  // The ONE End access on screen is the rail's control (the usage page's
  // main carries no logout of its own) — target it by its locale-stable
  // attribute, valid for both the en in-process lane and the zh executable.
  await page.locator('[data-shell-action="end-access"]').click();
  await page.locator('[data-native-state="access"]').waitFor();
  await logoutStarted;
  await page.evaluate(() => { window.dispatchEvent(new Event('focus')); document.dispatchEvent(new Event('visibilitychange')); window.dispatchEvent(new PopStateEvent('popstate')); });
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  assert.equal(readsDuringLogout, 0, `reads during logout: ${logoutReads.join(', ')}`);
  const ended = page.waitForResponse((response) => response.url().endsWith('/console/session') && response.request().method() === 'DELETE');
  releaseLogout(); await ended;
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await page.locator('[data-native-state="access"]').waitFor();
  assert.equal(readsDuringLogout, 0, `reads during logout: ${logoutReads.join(', ')}`);
  assert.equal(await page.locator('[data-kind]').count(), 0);
  assert.deepEqual(failures, []);
  console.log(config.executable ? 'PASS native executable browser without Node runtime PATH' : 'PASS bilingual retained browser, dynamic engagement, null evidence, privacy and logout');
} finally { await browser.close(); }
process.exit(0);
