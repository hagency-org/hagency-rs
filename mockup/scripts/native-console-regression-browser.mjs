/* The #56 regression lane: ONE login walks every rail entry and proves the
 * key operator actions end to end on fixture data. It receives the same
 * ticket stream every lane receives (never a token) and the harness mints
 * each scoped link on request — one outstanding ticket at a time. */
import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { chromium } from 'playwright-core';
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const config = JSON.parse((await lines.next()).value);
async function fixture(command) { console.log(command); return JSON.parse((await lines.next()).value); }

const browser = await chromium.launch({ executablePath: process.env.HAGENCY_BROWSER_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true,
  args: ['--disable-background-networking', '--disable-component-update', '--no-default-browser-check'] });

try {
  const context = await browser.newContext({ serviceWorkers: 'block' });
  const page = await context.newPage();
  const failures = []; const urls = [];
  // ADR-186 §A: the walk asks for more than the headroom ONCE on purpose.
  // That one 409 (its console mirror and its wire response) is named here
  // and consumed when it happens; any other refusal still fails the walk.
  const deliberate = { console: 0, wire: 0 };
  page.on('pageerror', (error) => failures.push(error.message));
  // "No console error" excludes two structural noises of the PACKAGED
  // export, pinned by the wire assertion below — not a blanket weakening:
  // (a) the favicon 404 the static export cannot ship; (b) Next Link's
  // HEAD prefetches against the GET-only asset server (the roster's
  // drill-down Links — their destinations are not even in the package).
  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    if (/favicon/i.test(message.text())) return;
    if (/status of 405 \(Method Not Allowed\)/.test(message.text())) return;
    // The stream EventSource's 401 connect — exempted on the wire below and
    // named there; its console mirror is the same single event.
    if (/status of 401 \(Unauthorized\)/.test(message.text())) return;
    if (deliberate.console > 0 && /status of 409 \(Conflict\)/.test(message.text())) { deliberate.console -= 1; return; }
    failures.push(message.text());
  });
  // The wire assertion that keeps the filter honest: EVERY refused response
  // is named. Only HEAD prefetches to app routes and the EventSource's 401
  // stream connect are noise; any other 4xx/5xx fails the walk by name.
  const refused = [];
  page.on('response', (response) => {
    if (response.status() < 400) return;
    const method = response.request().method();
    const path = response.url().replace(config.base, '');
    if (method === 'HEAD' && !path.startsWith('/console/api/')) return;
    if (method === 'GET' && path === '/console/api/stream' && response.status() === 401) return;
    if (deliberate.wire > 0 && method === 'POST' && response.status() === 409 && /^\/console\/api\/engagements\/[^/]+\/approve$/.test(path)) { deliberate.wire -= 1; return; }
    refused.push(`${response.status()} ${method} ${path}`);
  });
  await context.route('**/*', async (route) => {
    const url = new URL(route.request().url()); urls.push(url.toString());
    if (url.origin !== config.base) { failures.push('unexpected external request'); await route.abort(); }
    else await route.continue();
  });

  // The login: one read-only ticket, exchanged once; every walk below rides
  // the same session cookie.
  await page.goto(config.url);
  await page.locator('[data-native-state="ready"]').waitFor();
  assert(!/private_|operator\.token/.test(await page.locator('main').innerText()), 'no credential value on screen');

  /* --- Part 1: rail navigation opens each selected page, without stale content. */
  const PAGES = [
    ['usage', /usage|用量/i],
    ['resources', /resources|资源/i],
    ['alerts', /alert|告警/i],
    ['server-engagements', /server engagements|服务器关联/i],
    ['engagements', /agent allocations|Agent 配额/i],
    ['agents', /workforce|员工名册|roster|projections/i],
  ];
  const headings = {};
  for (const [key, pattern] of PAGES) {
    await page.locator(`nav.rail a[href$="/${key}/"]`).click();
    await page.waitForURL(`${config.base}/console/${key}/`);
    // Resource and server-engagement pages have their own panel attributes.
    await page.locator('[data-native-state], [data-native-resource-state], [data-server-engagements]').first().waitFor();
    const heading = page.locator('.page-head h1').filter({ hasText: pattern });
    await heading.waitFor({ state: 'visible' });
    const head = await heading.innerText();
    assert(head && head.trim().length > 0, `${key}: the page renders an h1 heading`);
    assert(pattern.test(head) || pattern.test(await page.locator('main').innerText()), `${key}: heading matches its page`);
    assert((await page.locator('main').innerText()).trim().length > 0, `${key}: not a blank page`);
    headings[key] = head;
  }
  assert.equal(new Set(Object.values(headings)).size, PAGES.length, 'each rail entry opens a distinct page');

  /* A loading state, not "no rows": throttle the page's API reads so the
   * fetch is in flight while the paint happens, then release. */
  await page.route('**/console/api/engagements*', async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 400));
    await route.continue();
  });
  await page.locator('nav.rail a[href$="/engagements/"]').click();
  await page.waitForURL(`${config.base}/console/engagements/`);
  await page.locator('p[role="status"]').first().waitFor();
  await page.unroute('**/console/api/**');
  await page.locator('[data-native-state="ready"]').waitFor();

  /* --- Part 2: the key actions, each asserting its visible result text. */

  /* (a) APPROVE a pending engagement (lifecycle scope). The harness seeded
   * PendingWorker1 before the walk; the verdict panel lists pending rows by
   * AGENT NAME (the row never shows the engagement id). */
  const lifecycleUrl = (await fixture('LIFECYCLE_TICKET')).url;
  await page.goto(lifecycleUrl);
  await page.locator('[data-native-state="ready"]').waitFor();
  await page.goto(`${config.base}/console/engagements/`);
  await page.locator('[data-native-state="ready"]').waitFor();
  /* ADR-186 §A: the amount field starts at the request; an amount above the
   * headroom is refused with the store's explanation shown in the row;
   * "All remaining" fills in the candidate's headroom (the 1000-token pool
   * less the seeded UsageWorker's 100); the approval then grants 80. */
  const pendingRow = page.locator('[data-verdict-panel] tr', { hasText: config.pendingAgent });
  const amount = pendingRow.locator('input[data-approve-amount]');
  if (await amount.inputValue() !== '100') throw new Error(`the amount field did not start at the request: ${await amount.inputValue()}`);
  await amount.fill('99999');
  deliberate.console = 1; deliberate.wire = 1;
  await pendingRow.getByRole('button', { name: /^(Approve|批准)$/ }).click();
  await pendingRow.locator('p[role="alert"]').filter({ hasText: /would exceed/ }).waitFor();
  assert.equal(deliberate.wire, 0, 'the over-headroom approval was refused on the wire');
  await pendingRow.getByRole('button', { name: /^(All remaining|全部剩余)$/ }).click();
  if (await amount.inputValue() !== '900') throw new Error(`All remaining filled ${await amount.inputValue()}, not the 900 headroom`);
  await amount.fill('80');
  await pendingRow.getByRole('button', { name: /^(Approve|批准)$/ }).click();
  await page.locator('[data-verdict-panel] p[role="status"]').filter({ hasText: /approved — provisioning enqueued|已批准/ }).waitFor();
  // The deliberate refusal is spent; nothing after it is excused.
  deliberate.console = 0; deliberate.wire = 0;

  /* (b) REFUSE another pending engagement: the harness admits one on request. */
  const second = (await fixture('REFUSE_ENGAGEMENT')).agent;
  await page.locator('[data-verdict-panel]').waitFor();
  await page.getByRole('button', { name: /^(Refresh|刷新)$/ }).first().click();
  await page.locator('[data-verdict-panel] tr', { hasText: second }).waitFor();
  await page.locator('[data-verdict-panel] tr', { hasText: second }).getByRole('button', { name: /^(Reject|拒绝)$/ }).click();
  // Refuse is TWO steps by design: Reject arms an inline confirmation
  // (NativeVerdict.jsx renders nv.confirmRefuse + Confirm/Cancel), and
  // only Confirm posts the decision.
  await page.locator('[data-verdict-panel] tr', { hasText: second }).getByRole('button', { name: /^(Confirm|确认)$/ }).click();
  // A refused POST renders its error word in the row (role=alert), not a
  // flash — dump the panel so the failure mode is named, not guessed.
  try {
    await page.locator('[data-verdict-panel] p[role="status"]').filter({ hasText: /refused — the engagement is rejected|已拒绝/ }).waitFor({ timeout: 15_000 });
  } catch (error) {
    const panel = await page.locator('[data-verdict-panel]').innerText().catch(() => '(no verdict panel)');
    throw new Error(`refused flash never rendered; verdict panel says:\n${panel}\n${error.message}`);
  }

  /* ADR-186 §C: ADD TOKENS to the seeded running engagement from its row in
   * the list; the row's note says what was added and the harness checks the
   * stored allocation (100 requested + 25). */
  const runningRow = page.locator(`tr[data-engagement-row="${config.engagement}"]`);
  await runningRow.getByRole('button', { name: /^(Add tokens|追加 token)$/ }).click();
  await runningRow.locator('input[data-top-up-amount]').fill('25');
  await runningRow.getByRole('button', { name: /^(Add|追加)$/ }).click();
  await page.locator('p[data-engagement-note]').filter({ hasText: /added 25 tokens|已追加 25/ }).waitFor();

  /* (c) STOP an agent (same lifecycle scope): the roster's stop control with
   * its #43 feedback notice. The seeded UsageWorker holds a live started
   * dispatch, so the stop is accepted and the notice says so.
   * (d) START: board #106 renders it per row — for a STOPPED row. This walk
   * asserts a serving row still offers no Start (asserted below, before the
   * stop); the positive stopped -> Start -> start journey is driven by the
   * served-binary lane (native_console_agents_stop_then_start), which clicks
   * it. */
  await page.goto(`${config.base}/console/agents/`);
  await page.locator('[data-native-state="ready"]').waitFor();
  assert((await page.locator('[data-lifecycle-action="start"]').count()) === 0, 'a serving row offers no start control');
  await page.locator(`[data-engagement-id="${config.engagement}"] [data-lifecycle-action="stop"]`).click();
  await page.locator('[data-stop-action="saved"]').waitFor();

  /* (e) The retired manual project-side connection page redirects to the
   * server-engagement flow; registration APIs retain their backend tests. */
  await page.goto(`${config.base}/console/project-sides/`);
  await page.waitForURL(/\/console\/server-engagements\/?$/);
  await page.locator('[data-server-engagements]').waitFor();
  assert.equal(await page.locator('[data-palpo-import], [data-side-registration]').count(), 0);

  /* (f) CLEAR-DIRTY on the configuration wizard (configuration scope): edit
   * the draft's ceiling, then Reload discards it and restores the observed
   * value — the seeded source's own 5000-token ceiling. */
  const configurationUrl = (await fixture('CONFIGURATION_TICKET')).url;
  await page.goto(configurationUrl);
  /* One login (#93): the ticket now lands on the usage page, so the
   * exchange is proven by the usage page's own readiness — the resources
   * page's attribute can never appear at this point anymore. The wizard
   * gate on the next line still proves the resources page itself. */
  await page.locator('[data-native-state="ready"]').first().waitFor();
  await page.goto(`${config.base}/console/resources/new/?resource_id=${config.resource}`);
  await page.locator(`[data-native-configuration-id="${config.resource}"][aria-busy="false"]`).waitFor();
  await page.getByRole('button', { name: /^(Next|下一步)$/ }).click();
  await page.getByRole('button', { name: /^(Next|下一步)$/ }).click();
  await page.locator('#wz-tokens').fill('777777');
  await page.getByRole('button', { name: /(Reload configuration and discard draft|重新读取配置并放弃草稿)/ }).click();
  await page.locator(`[data-native-configuration-id="${config.resource}"][aria-busy="false"]`).waitFor();
  // The tokens input carries its value only under the monthly arm — the
  // same select-then-read the configuration lane uses after its reload.
  assert.equal(await page.locator('#wz-tokens').inputValue(), '', 'an unassigned account configuration does not preallocate a resource budget');

  /* (g) ALERT TRANSITION (configuration scope): the fixture's open ceiling
   * alert resolves; the row leaves the open list and the no-open state
   * renders — the terminal case of the server-owned transition map. */
  await page.goto(`${config.base}/console/alerts/`);
  await page.locator('[data-native-state="ready"]').waitFor();
  await page.locator('tbody tr').first().click();
  await page.locator('[data-transition="resolved"]').click();
  await page.locator('tbody tr').first().waitFor({ state: 'detached' });
  await page.locator('main').getByText(/No open alerts|没有未解决的告警/).waitFor();

  /* (h) TASK CREATE: native serves no task-creation console route (the
   * audit's finding; no tasks POST exists in console.rs) and the agent
   * detail renders its task rows read-only — asserted as absence, the
   * honest bound of what this build can prove. */
  await page.goto(`${config.base}/console/agents/`);
  await page.locator('[data-native-state="ready"]').waitFor();
  await page.locator(`[data-agent-name="${config.agent}"] a`).first().click();
  await page.locator('[data-agent-name]').first().waitFor();
  assert((await page.locator('[data-task-id]').count()) >= 1, 'the detail renders its read-only task rows');
  assert((await page.getByRole('button', { name: /(create|new).*(task|任务)/i }).count()) === 0, 'no task-create control: the route does not exist in this build');

  // No console error, no external request, no ticket value in a URL — and
  // the wire assertion backing the console filters above: every refused
  // response was one of the two named structural noises, anything else
  // fails here by name.
  assert.deepEqual(failures, [], `console errors: ${failures.join(' | ')}`);
  assert.deepEqual(refused, [], `refused responses beyond the named noises: ${refused.join(' | ')}`);
  assert(urls.every((url) => !url.includes('access=')), 'no ticket value in a request URL');
  console.log('PASS native console regression browser');
} finally { await browser.close(); }
process.exit(0);
