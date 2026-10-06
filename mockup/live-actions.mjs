/*
 * Live console ACTION walk — the operator's real journey, through the UI only.
 *
 * live-walk.mjs LOOKS: it visits pages and records what rendered. This one DOES:
 * every step clicks and types the way an operator does, and no step calls an API
 * directly — the console's own client is the only thing that talks to the
 * service. That distinction is the point: a page can render perfectly and still
 * refuse the action, and only a real press finds out.
 *
 * Protocol: one JSON line on stdin {base,url,shots,projectManager} and an
 * optional serverEngagementId. projectManager is a full Matrix user ID on
 * the selected server; the resource wizard records it as an eligible manager.
 * The access link is never an argv, never printed, never logged — it carries the
 * one outstanding ticket, and the harness that minted it owns it.
 *
 * Every step is independently reported and a FAILURE DOES NOT STOP THE WALK:
 * a step that cannot run (no pending invitation, no alert, no running agent)
 * reports `ok:false` with the reason, and the walk continues, so one operator
 * journey yields the whole state of the console rather than the first obstacle.
 * Each report is {step, ok, detail, screenshot} and is emitted as one JSON line
 * as soon as it is known, so a crash later still leaves the earlier steps.
 *
 * Run against the fake harness (feature-gated suite spawns this file) or a live
 * rig: echo '{"base":"http://127.0.0.1:PORT","url":"<access link>","shots":"/tmp","projectManager":"@owner:example.org"}' | node live-actions.mjs
 */

import { createInterface } from 'node:readline';
import { chromium } from 'playwright-core';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

const line = await new Promise((r) => {
  const rl = createInterface({ input: process.stdin });
  rl.on('line', (l) => { rl.close(); r(l); });
});
const cfg = JSON.parse(line);
const shots = cfg.shots ?? '/tmp/hagency-live-actions';
await mkdir(shots, { recursive: true });

const browser = await chromium.launch({
  executablePath: process.env.HAGENCY_BROWSER_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
  args: ['--disable-background-networking', '--disable-component-update', '--no-default-browser-check'],
});
const context = await browser.newContext({ serviceWorkers: 'block', viewport: { width: 1360, height: 900 } });
const page = await context.newPage();

const jsErrors = [];
const apiErrors = [];
/* The console client's own reads and writes, bodies included. A page that
 * stays on its error panel says only "could not be read"; the response body
 * says WHICH field the client refused, which is the difference between a
 * product defect and a driver mistake. */
const apiBodies = [];
page.on('pageerror', (e) => jsErrors.push(String(e).slice(0, 200)));
page.on('console', (m) => { if (m.type() === 'error') jsErrors.push('console: ' + m.text().slice(0, 200)); });
page.on('response', async (r) => {
  const u = r.url();
  const path = u.replace(cfg.base, '').split('?')[0];
  const isApi = u.includes('/console/api/');
  // EVERY refused response, not only /console/api/: a 405 on the task create
  // and a 404 on a hashed chunk look identical in a console line, and the whole
  // point of the walk is to name what the service refused.
  if (r.status() >= 400) apiErrors.push(`${r.status()} ${r.request().method()} ${path}`);
  if (!isApi) return;
  let body = '';
  try { body = await r.text(); } catch { return; /* body gone (redirect/abort) */ }
  apiBodies.push(`${r.status()} ${r.request().method()} ${path} ${body.replace(/\s+/g, ' ').slice(0, 1400)}`);
  // A write the walk performed but the page never reflected is otherwise
  // invisible: state the response itself.
  if (path.includes('/comments')) console.log(`COMMENT_RESPONSE ${r.status()} ${body.replace(/\s+/g, ' ').slice(0, 400)}`);
});

/* The reason a failure happened, in the page's own words. A bare "click timed
 * out" is the failure mode that needs a re-run to diagnose; this names what was
 * actually on screen. */
async function visible() {
  return (await page.locator('main, body').first().innerText().catch(() => '')).replace(/\s+/g, ' ').slice(0, 300);
}
async function shot(name) {
  const path = join(shots, `${name}.png`);
  await page.screenshot({ path, fullPage: true }).catch(() => {});
  return path;
}
function report(step, ok, detail, screenshot, gap) {
  const line = { step, ok, detail, screenshot };
  // A failure that the walk can NAME is a declared gap (the fake cannot
  // exercise it, or the product cannot perform it at all) rather than an
  // unexplained break — the harness requires the distinction.
  if (!ok && gap) line.gap = gap;
  process.stdout.write(JSON.stringify(line) + '\n');
}

/* Each step: run it, report, keep going. `body` returns a detail string on
 * success; throwing fails only this step. `gap` names the limitation that makes
 * a failure of this step legitimate — omit it and a failure IS the defect. */
async function step(name, body, gap) {
  jsErrors.length = 0; apiErrors.length = 0; apiBodies.length = 0;
  try {
    const detail = await body();
    report(name, true, detail ?? '', await shot(name));
  } catch (error) {
    // Name the driver line that failed: "locator.waitFor timed out" alone
    // cannot say WHICH wait, and a bare message is the re-run-to-diagnose
    // failure mode this walk exists to end.
    const where = (error.stack ?? '').split('\n').map((l) => l.trim()).find((l) => l.includes('live-actions.mjs')) ?? '(no driver frame)';
    // One line per ENDPOINT (first sighting), not the last four bodies: the
    // question after a failed step is which calls the step actually made, and
    // a page that re-reads in a loop otherwise buries the write that never
    // happened.
    const seen = new Set();
    const endpoints = apiBodies.filter((b) => {
      const key = b.split(' ').slice(0, 3).join(' ');
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
    const reason = `${error.message.split('\n')[0]}; at ${where}; apiErrors=[${apiErrors.join(', ')}]; api=[${endpoints.join(' || ')}]; js=[${jsErrors.slice(0, 2).join('; ')}]; page=${JSON.stringify(await visible())}`;
    report(name, false, reason, await shot(`${name}-FAILED`), gap);
  }
}

/* The one happy-state gate every action page shares: the console's own client
 * has fetched and the page is ready (not loading, not the access wall). */
const ready = () => page.locator('[data-native-state="ready"]').first().waitFor({ timeout: 20_000 });
const name = (re) => page.getByRole('button', { name: re }).first();

// ---------------------------------------------------------------------------
// 1. Log in via the access link. The link lands on the console with the ticket
//    in the fragment; the console exchanges it for a session, so "logged in" is
//    exactly "a ready page rendered" (and NOT the access wall).
// ---------------------------------------------------------------------------
await step('1-login', async () => {
  await page.goto(cfg.url, { waitUntil: 'domcontentloaded' });
  await ready();
  const wall = await page.locator('[data-native-state="access"]').count();
  if (wall) throw new Error('the access wall rendered after the access link');
  return 'the console exchanged the access link and rendered a ready page';
});

// ---------------------------------------------------------------------------
// 2. Create a resource configuration (the new-resource wizard) and see it listed.
//    Creating needs a SOURCE resource to derive from, so the wizard is opened
//    from the resources list's own create link — the path an operator takes.
// ---------------------------------------------------------------------------
await step('2-create-resource-configuration', async () => {
  await page.goto(`${cfg.base}/console/resources/`);
  await page.locator('[data-native-resource-state="ready"][aria-busy="false"]').waitFor();
  const source = await page.locator('[data-resource-row]').first().getAttribute('data-resource-row');
  if (!source) throw new Error('no source resource to derive a configuration from');
  const before = await page.locator('[data-resource-row]').count();
  await page.goto(`${cfg.base}/console/resources/new/?source_resource_id=${encodeURIComponent(source)}`);
  await page.locator('[data-native-configuration-id]').waitFor();
  await page.locator('#configuration-engagement option').nth(1).waitFor({ state: 'attached' });
  await page.locator('#configuration-engagement').selectOption(cfg.serverEngagementId ?? { index: 1 });
  // Two Next clicks reach the budget step (NATIVE_STEPS is model, reasoning,
  // budget); the name is prefilled from the source, and a monthly ceiling is
  // the one shape `save` accepts natively.
  await name(/^(Next|下一步)$/).click();
  await name(/^(Next|下一步)$/).click();
  await page.locator('#wz-tokens').fill('40000');
  if (!cfg.projectManager) throw new Error('projectManager must name an eligible Matrix user on the selected server');
  await page.locator('#configuration-managers').fill(cfg.projectManager);
  await name(/^(Create resource|创建资源)$/).click();
  await page.locator('[data-configuration-action="saved"]').waitFor();
  await page.goto(`${cfg.base}/console/resources/`);
  await page.locator('[data-native-resource-state="ready"][aria-busy="false"]').waitFor();
  const after = await page.locator('[data-resource-row]').count();
  if (after <= before) throw new Error(`the created configuration is not listed (${before} -> ${after} rows)`);
  return `created from source ${source}; the list grew ${before} -> ${after} resources`;
});

// ---------------------------------------------------------------------------
// 3. Open engagements, approve ONE pending request if any (else say so).
// ---------------------------------------------------------------------------
await step('3-approve-pending-engagement', async () => {
  await page.goto(`${cfg.base}/console/engagements/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  // The verdict controls exist only on a pending row; the driver clicks the
  // first Approve it can see, and reports honestly when there is none.
  const approve = page.getByRole('button', { name: /^(Approve|批准)$/ });
  if ((await approve.count()) === 0) return 'none pending';
  await approve.first().click();
  await page.locator('[data-native-state="ready"]').first().waitFor();
  return 'approved one pending request';
});

// ---------------------------------------------------------------------------
// 4. Agents: stop one running agent, see the state change, start it again.
//    Both halves run since board #106: the roster renders Stop for a serving
//    agent and Start for a stopped one (TS parity `backend-v2.js:6872,12712`),
//    so the whole journey is driven through the UI here.
// ---------------------------------------------------------------------------
await step('4-agents-stop-then-start', async () => {
  await page.goto(`${cfg.base}/console/agents/`);
  await ready();
  const stop = page.locator('[data-lifecycle-action="stop"]');
  if ((await stop.count()) === 0) throw new Error('no stop control rendered (no lifecycle authority?)');
  // The stop control lives in its row's lifecycle cell; the first row carrying
  // one is the agent this step stops. `filter({has})` resolves the inner
  // locator PER ROW, so it must be the bare locator — `.first()` would match
  // every row (each has a first stop) and trip strict mode.
  const row = page.locator('tbody tr').filter({ has: stop }).first();
  const engagement = await row.getAttribute('data-engagement-id');
  if (!engagement) throw new Error('the lifecycle row has no engagement binding');
  const before = (await row.innerText()).replace(/\s+/g, ' ');
  await stop.first().click();
  await page.locator('[data-stop-action="saved"], [data-stop-action="refused"], [data-stop-action="unknown"]').waitFor({ timeout: 20_000 });
  const outcome = await page.locator('[data-stop-action]').first().getAttribute('data-stop-action');
  if (outcome !== 'saved') throw new Error(`storing the stop returned "${outcome}" (row was ${JSON.stringify(before.slice(0, 100))})`);
  // The stop is an awaited mutation that refreshes the roster itself, so the
  // state change to observe is the row's own return to serving.
  await ready();
  // The mutation receipt can render before the follow-up roster fetch. Wait
  // for this exact agent's new control, not another ready panel or agent row.
  const currentRow = page.locator(`tbody tr[data-engagement-id="${engagement}"]`);
  const start = currentRow.locator('[data-lifecycle-action="start"]');
  await start.waitFor({ state: 'visible', timeout: 20_000 });
  await start.click();
  await page.locator('[data-start-action="saved"], [data-start-action="refused"], [data-start-action="unknown"]').waitFor({ timeout: 20_000 });
  const restarted = await page.locator('[data-start-action]').first().getAttribute('data-start-action');
  if (restarted !== 'saved') throw new Error(`the start returned "${restarted}"`);
  await ready();
  await currentRow.locator('[data-lifecycle-action="stop"]').waitFor({ state: 'visible', timeout: 20_000 });
  return 'stopped an agent and started it again';
});

// ---------------------------------------------------------------------------
// 5. Tasks: create one, comment on it, transition it, delete it.
// ---------------------------------------------------------------------------
await step('5-tasks-create-comment-transition-delete', async () => {
  await page.goto(`${cfg.base}/console/tasks/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  if ((await name(/^(New task|新建任务)$/).count()) === 0) throw new Error('the create control did not render (no configure scope?)');
  const title = `live-actions ${new Date().toISOString()}`;
  await name(/^(New task|新建任务)$/).click();
  await page.locator('label', { hasText: /^(Title|标题)$/ }).locator('input').fill(title);
  await name(/^(Create|创建)$/).click();
  const row = page.locator('tbody tr', { hasText: title });
  try {
    await row.first().waitFor({ timeout: 20_000 });
  } catch (error) {
    const titles = await page.locator('tbody tr td:first-child').allInnerTexts().catch(() => []);
    throw new Error(`the created task never appeared in the list; rows now: ${JSON.stringify(titles.slice(0, 8))}`);
  }
  await row.first().click();
  await page.locator('h3', { hasText: title }).waitFor({ timeout: 20_000 });
  const box = page.getByPlaceholder(/^(Add a comment…|添加评论…)$/);
  const commentButtons = page.getByRole('button', { name: /^(Comment|评论)$/ });
  if ((await box.count()) !== 1) throw new Error(`expected exactly one comment input, found ${await box.count()}`);
  await box.fill('operator note from live-actions');
  // A disabled control means the fill never reached the page's own state, and a
  // silent no-op click is exactly the failure this walk must NAME rather than
  // report as a bare timeout.
  const before = { buttons: await commentButtons.count(), value: await box.inputValue(), disabled: await commentButtons.first().isDisabled() };
  if (before.disabled) throw new Error(`the comment control is disabled after filling it: ${JSON.stringify(before)}`);
  await commentButtons.first().click();
  try {
    await page.locator('dl', { hasText: 'operator note from live-actions' }).waitFor({ timeout: 20_000 });
  } catch (error) {
    const sent = apiBodies.filter((b) => b.includes('/comments')).join(' || ') || 'NO /comments REQUEST WAS SENT';
    // `act` writes its caught error to the page's one announcer (`Toast`,
    // role=status), so the toast IS the reason the write did not happen.
    const toast = await page.locator('[role="status"]').allInnerTexts().catch(() => []);
    const panel = await page.locator('h3').allInnerTexts().catch(() => []);
    throw new Error(`the comment never rendered back; before-click ${JSON.stringify(before)}; sent: ${sent}; toast=${JSON.stringify(toast)}; headings=${JSON.stringify(panel)}`);
  }
  // Transition: the panel offers exactly the statuses the SERVER allows from
  // the current one, so the first "→ x" control is a legal move by definition.
  const move = page.getByRole('button', { name: /^→ / }).first();
  if ((await move.count()) === 0) throw new Error('no legal transition offered for the new task');
  await move.click();
  await page.locator('tbody tr', { hasText: title }).first().click();
  page.once('dialog', (d) => d.accept()); // tk.confirmDelete
  await name(/^(Delete|删除)$/).click();
  await page.locator('tbody tr', { hasText: title }).waitFor({ state: 'detached', timeout: 20_000 });
  return `created, commented, transitioned and deleted ${JSON.stringify(title)}`;
});

// ---------------------------------------------------------------------------
// 6. Alerts: acknowledge one if any.
// ---------------------------------------------------------------------------
await step('6-acknowledge-alert', async () => {
  await page.goto(`${cfg.base}/console/alerts/`);
  const state = page.locator('[data-native-state]').first();
  try {
    await page.locator('[data-native-state="ready"]').first().waitFor({ timeout: 20_000 });
  } catch (error) {
    // The alerts page reuses the usage error panel's words, so a read failure
    // is otherwise indistinguishable from "no alerts" — name what it said.
    const observed = await state.getAttribute('data-native-state').catch(() => null);
    const panel = await page.locator('main').innerText().catch(() => '');
    // Which check the client refused, computed from the body IT received: the
    // validator is exact-key and the page's own words never say WHICH field.
    const last = apiBodies.filter((b) => b.includes('/console/api/alerts')).pop() ?? '(no alerts read seen)';
    let why = 'n/a';
    try {
      const json = JSON.parse(last.slice(last.indexOf('{')));
      const alert = json.alerts?.[0];
      const DETAIL_KEYS = ['agent', 'presetId', 'ceilingTokens', 'committedTokens', 'measuredTokens', 'drawnTokens', 'overByTokens'];
      const ALERT_KEYS = ['dedupe_key', 'resource_id', 'summary', 'detail', 'runbook', 'impact', 'recovery_condition', 'occurrences', 'first_seen_ms', 'last_seen_ms', 'resolved', 'severity', 'status', 'next', 'note'];
      why = JSON.stringify({
        topKeys: Object.keys(json),
        alertKeys: alert ? Object.keys(alert) : null,
        missingAlertKeys: alert ? ALERT_KEYS.filter((k) => !Object.hasOwn(alert, k)) : null,
        detailKeys: alert ? Object.keys(alert.detail ?? {}) : null,
        missingDetailKeys: alert && alert.detail && typeof alert.detail === 'object' ? DETAIL_KEYS.filter((k) => !Object.hasOwn(alert.detail, k)) : null,
        detailValues: alert?.detail ?? null,
      });
    } catch (e) { why = `could not parse: ${e.message}`; }
    throw new Error(`the alerts read never reached ready (data-native-state=${JSON.stringify(observed)}); validator=${why}; panel=${JSON.stringify(panel.replace(/\s+/g, ' ').slice(0, 200))}`);
  }
  const ack = page.locator('[data-transition="acknowledged"]');
  if ((await ack.count()) === 0) return 'none open';
  await ack.first().click();
  await page.locator('[data-transition="acknowledged"]').first().waitFor({ state: 'detached', timeout: 20_000 });
  return 'acknowledged one alert';
});

// ---------------------------------------------------------------------------
// 7. Usage renders numbers (not the word "unknown", not an empty page).
// ---------------------------------------------------------------------------
await step('7-usage-renders-numbers', async () => {
  await page.goto(`${cfg.base}/console/usage/`);
  await page.locator('[data-native-state="ready"]').first().waitFor();
  /*
   * The FLEET half of the same page (board #108). Its read is a COMPOSITION —
   * totals, then one budget per side — so a single failing side replaces the
   * whole panel with "Fleet usage could not be read" while the per-engagement
   * `[data-kind]` cells above still render numbers. That is precisely how a
   * client-side budget validator rejected every read and this step still
   * reported clean, so the count above is not enough: require the panel's own
   * figure, never its failure panel.
   */
  const fleetDrawn = page.locator('[data-fleet="drawn"]');
  try {
    await fleetDrawn.first().waitFor({ state: 'visible', timeout: 10_000 });
  } catch {
    const alert = (await page.locator('section.panel[role="alert"] h2').allInnerTexts()).join('; ');
    throw new Error(`the fleet panel rendered no figures (panel said: ${alert || '(nothing)'})`);
  }
  const fleetText = (await fleetDrawn.first().innerText()).trim();
  if (fleetText === '') throw new Error('the fleet panel rendered an empty drawn figure');
  /*
   * The ENGAGEMENT half (board #115). The page OPENS on the first engagement
   * of the list, and a fleet's first engagement is as likely to be one that
   * never ran a turn as one that did — the live rig's RustFleetCoordinator had
   * never been dispatched, so every kind in its own summary legitimately read
   * Unknown while the old version of this step still passed on the fleet
   * figure alone. So walk the selector the operator walks, take the first
   * engagement whose OWN evidence renders numbers, and assert those numbers —
   * `[data-engagement-id] > section:first-of-type` is that engagement's
   * summary (the ceiling block below it is the RESOURCE's figure, not the
   * engagement's, which is exactly the confusion board #115 reported). A
   * fleet where nothing was measured says so by name.
   */
  const candidates = await page
    .locator('#native-engagement option')
    .evaluateAll((options) => options.map((option) => option.value).filter(Boolean));
  if (candidates.length === 0) throw new Error('no engagement with usage (the list is empty)');
  let chosen = null;
  let summary = null;
  for (const id of candidates) {
    await page.locator('#native-engagement').selectOption(id);
    try {
      await page.locator(`[data-engagement-id="${id}"]`).waitFor({ timeout: 4_000 });
    } catch {
      continue; // its own read failed or is still in flight; try the next
    }
    const cells = await page.locator(`[data-engagement-id="${id}"] > section:first-of-type [data-kind]`).allInnerTexts();
    const numeric = cells.filter((word) => /\d/.test(word)).length;
    if (numeric > 0) {
      chosen = id;
      summary = { numeric, total: cells.length };
      break;
    }
  }
  if (chosen === null) throw new Error(`no engagement with usage (checked ${candidates.length})`);
  const input = (await page.locator(`[data-engagement-id="${chosen}"] > section:first-of-type [data-kind="input"]`).first().innerText()).trim();
  if (!/\d/.test(input)) throw new Error(`the chosen engagement's summary rendered a non-numeric input count: ${JSON.stringify(input)}`);
  return `engagement ${chosen} renders measured numbers (input "${input}", ${summary.numeric} of ${summary.total} summary cells); fleet drawn "${fleetText}"`;
});

// ---------------------------------------------------------------------------
// 8. End access and confirm the console asks for a link again.
// ---------------------------------------------------------------------------
await step('8-end-access', async () => {
  await page.locator('[data-shell-action="end-access"]').click();
  // The console asks for a link again by rendering its access section — the
  // heading is the OUTCOME word once a sign-out has run (`nr.logout.ended`),
  // which is why the reference drivers assert `data-logout-state`, not the
  // entry heading `nu.access`.
  const wall = page.locator('[data-native-state="access"]').first();
  await wall.waitFor({ timeout: 20_000 });
  const state = await wall.getAttribute('data-logout-state');
  if (!state) throw new Error('the access section rendered without a sign-out outcome');
  return `the console asks for a link again (data-logout-state="${state}")`;
});

await browser.close();
process.exit(0);
