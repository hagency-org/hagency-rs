/* Closed native browser protocol. Credentials exist only in the fragment exchange
 * and HttpOnly cookie; usage facts never enter local/session storage. */
import { remember } from './labels';

export const NATIVE_MODE = process.env.NEXT_PUBLIC_HAGENCY_NATIVE_CONSOLE === '1';
export function serverEngagementsView(location) { return /^\/console\/server-engagements\/?$/.test(location.pathname); }
const ROOT = '/console';

/* Build-time constants (ADR-145): the workspace version and the binary's
 * EXPECTED schema head. build-native-console.mjs appends reassignments to
 * the staged copy of this module inside the build's mkdtemp tree — no repo
 * path is generated and nothing enters manifest.json. The repo copy keeps
 * these neutral defaults; the strip renders them verbatim, never invents. */
export let HAGENCY_NATIVE_VERSION = 'unknown';
export let HAGENCY_NATIVE_SCHEMA_HEAD = 0;
const KINDS = ['input', 'output', 'cacheWrite', 'cacheRead'];
const STATES = ['pending', 'reserved', 'active', 'rejected', 'revoked', 'failed'];
const CLEANUP = ['not_required', 'pending', 'uncertain', 'complete'];
const id = (v) => typeof v === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(v);
const number = (v) => Number.isSafeInteger(v) && v >= 0;
const object = (v, keys) => v !== null && typeof v === 'object' && !Array.isArray(v)
  && Object.keys(v).length === keys.length && keys.every((k) => Object.hasOwn(v, k));
const counts = (v, nullable) => object(v, KINDS) && KINDS.every((k) => number(v[k]) || (nullable && v[k] === null));
const evidence = (v) => v === 'host_attributed_untrusted_usage';
const period = (v) => v === null || (object(v, ['kind', 'key', 'observed_growth', 'known_growth_lower_bound', 'incomplete', 'observations', 'evidence'])
  && ['daily', 'monthly'].includes(v.kind) && typeof v.key === 'string' && v.key.length <= 10
  && counts(v.observed_growth, true) && counts(v.known_growth_lower_bound, false)
  && typeof v.incomplete === 'boolean' && number(v.observations) && evidence(v.evidence));

// Ceiling headroom published with the report (ADR-123): the drawn figure is
// always known; used and remaining are null when nothing was measured or no
// limit is declared. Unknown is rendered as unknown, never as zero.
const headroom = (v) => object(v, ['tokens_drawn', 'tokens_used', 'remaining_tokens']) && number(v.tokens_drawn)
  && (v.tokens_used === null || number(v.tokens_used)) && (v.remaining_tokens === null || number(v.remaining_tokens));

export function validateReport(v, selected) {
  const s = v?.summary;
  if (!object(v, ['engagement_id', 'at_ms', 'summary', 'daily', 'monthly', 'ceiling']) || v.engagement_id !== selected || !number(v.at_ms)
    || !headroom(v.ceiling)
    || !object(s, ['sources', 'latest_counts', 'known_high_water_lower_bound', 'latest_incomplete_sources', 'historically_incomplete_sources', 'regression_observations', 'evidence'])
    || !['sources', 'latest_incomplete_sources', 'historically_incomplete_sources', 'regression_observations'].every((k) => number(s[k]))
    || !(s.latest_counts === null || counts(s.latest_counts, true))
    || !(s.known_high_water_lower_bound === null || counts(s.known_high_water_lower_bound, false))
    || !evidence(s.evidence) || !period(v.daily) || !period(v.monthly)) throw new Error('invalid_native_response');
  return v;
}
function validMatrixProfile(p) {
  const label = (s) => typeof s === 'string' && [...s].length > 0 && [...s].length <= 128;
  return object(p, ['desiredName', 'observedName', 'state', 'lastError', 'observedAtMs'])
    && label(p.desiredName) && (p.observedName === null || label(p.observedName))
    && ['pending', 'failed', 'verified'].includes(p.state)
    && (p.lastError === null || p.lastError === 'matrix_profile_unverified')
    && (p.observedAtMs === null || number(p.observedAtMs));
}
export function validateEngagements(v) {
  /* projectName is bounded in Unicode SCALAR VALUES (code points), not JS
   * string length (UTF-16 code units): the server truncates at verification
   * time with trim().chars().take(255) (authority.rs:286), so a name of 255
   * astral characters is 510 UTF-16 units. Counting code points here
   * ([...s].length) keeps the two bounds in the same unit — a .length check
   * would refuse the whole read over a name the server legitimately stored
   * (E4 of the engagements review). The retained JS bound
   * (backend-v2.js:8061, slice(0,255)) counts UTF-16 units, but it is an
   * implementation accident of `slice`, not a designed rule; the native
   * verifier's scalar bound is the contract. */
  if (!object(v, ['engagements', 'next_after']) || !Array.isArray(v.engagements) || v.engagements.length > 16
    || !(v.next_after === null || id(v.next_after)) || v.engagements.some((e) => !object(e, ['id', 'agentName', 'projectName', 'role', 'requestedTokens', 'state', 'cleanup', 'agentRemainingTokens', 'ownerBindingRequired', 'createdAtMs', 'endedAtMs', 'allocatedTokens', 'spentTokens', 'quotaPaused', ...(Object.hasOwn(e, 'coordinatorManaged') ? ['coordinatorManaged'] : []), ...(Object.hasOwn(e, 'matrixProfile') ? ['matrixProfile'] : [])]) || (Object.hasOwn(e, 'coordinatorManaged') && typeof e.coordinatorManaged !== 'boolean')
      || (Object.hasOwn(e, 'matrixProfile') && !validMatrixProfile(e.matrixProfile))
      || !id(e.id) || typeof e.agentName !== 'string' || e.agentName.length > 128
      || !(e.projectName === null || (typeof e.projectName === 'string' && [...e.projectName].length <= 255))
      || typeof e.role !== 'string' || e.role.length > 128 || !number(e.requestedTokens) || !STATES.includes(e.state) || !CLEANUP.includes(e.cleanup)
      /* Board #60 item 3: the remaining allowance is null when no ceiling is
       * declared (unknown, never a zero allowance), and the owner-binding
       * flag is a real boolean — never coerce an absent one to false. */
      || !(e.agentRemainingTokens === null || number(e.agentRemainingTokens))
      || typeof e.ownerBindingRequired !== 'boolean'
      || !(e.createdAtMs === null || number(e.createdAtMs))
      || !(e.endedAtMs === null || number(e.endedAtMs))
      /* ADR-186: the allocation held, the known spend (null while unknown,
       * never a zero) and the quota hold. */
      || !number(e.allocatedTokens)
      || !(e.spentTokens === null || number(e.spentTokens))
      || typeof e.quotaPaused !== 'boolean')) throw new Error('invalid_native_response');
  return v;
}
const RECOVERY_ERRORS = { agent_lifecycle_scope_required: 403, resolution_conflict: 409, dispatch_not_resolvable: 409, invalid_console_request: 400 };
/* End access revokes the credential this page holds. From the moment it
 * starts until a new ticket is exchanged, no console read or write leaves the
 * page: a multi-step load already in flight (the fleet panel's per-side budget
 * reads) would otherwise keep presenting the revoked cookie. */
let accessEnded = false;
async function request(path, options = {}, responseLimit = 64 * 1024) {
  if (accessEnded && path !== '/session') throw new Error('console_access_required');
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 5000);
  try {
    const response = await fetch(`${ROOT}${path}`, { ...options, credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: abort.signal });
    if (!response.headers.get('content-type')?.startsWith('application/json')) throw new Error('invalid_native_response');
    const reader = response.body.getReader();
    const chunks = []; let size = 0;
    for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      size += value.length;
      if (size > responseLimit) { await reader.cancel(); throw new Error('invalid_native_response'); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    if (!response.ok) {
      if (value?.code === 'console_busy' && response.status === 429) throw new Error('busy');
      const known = { busy: 503, outcome_unknown: 504, resource_revision_conflict: 409, resource_publication_scope_required: 403, resource_configuration_scope_required: 403, resource_in_use: 409, invalid_resource_command: 400, account_scope_required: 403, account_state_conflict: 409, account_revision_conflict: 409, invalid_account_command: 400, invalid_engagement_id: 400, engagement_not_live: 409, engagement_not_pending: 409, decision_conflict: 409, over_commit: 409, no_ceiling: 409, insufficient_capacity: 409, agent_unavailable: 409, registration_generation: 409, command_conflict: 409, stale_generation: 409, invalid_side_query: 400, sides_unavailable: 503 };
      if (known[value?.code] === response.status || RECOVERY_ERRORS[value?.code] === response.status) {
        // ADR-186 §A2: a refusal may carry the store's human explanation of
        // the binding limit beside its code; it rides the error as `detail`.
        const refused = new Error(value.code);
        if (typeof value.message === 'string' && value.message.length > 0 && value.message.length <= 2048) refused.detail = value.message;
        throw refused;
      }
      throw new Error(response.status === 401 ? 'console_access_required' : (response.status === 404 ? 'not_found' : 'native_unavailable'));
    }
    return value;
  } catch (error) {
    if (['console_access_required', 'not_found', 'invalid_native_response', 'invalid_selection', 'busy', 'outcome_unknown', 'resource_revision_conflict', 'resource_publication_scope_required', 'resource_configuration_scope_required', 'resource_in_use', 'invalid_resource_command', 'account_scope_required', 'account_state_conflict', 'account_revision_conflict', 'invalid_account_command', 'invalid_engagement_id', 'engagement_not_live', 'engagement_not_pending', 'decision_conflict', 'over_commit', 'no_ceiling', 'insufficient_capacity', 'agent_unavailable', 'registration_generation', 'command_conflict', 'stale_generation', 'invalid_side_query', 'sides_unavailable', ...Object.keys(RECOVERY_ERRORS)].includes(error.message)) throw error;
    if (options.method === 'DELETE') throw new Error('logout_unknown');
    if (['POST', 'PATCH'].includes(options.method) && (path.startsWith('/api/resources') || path.startsWith('/api/accounts') || path.startsWith('/api/agents/') || path.startsWith('/api/engagements/'))) throw new Error('outcome_unknown');
    throw new Error('native_unavailable');
  } finally { clearTimeout(timer); }
}
export { request as nativeRequest };
/* ADR-145: the readiness payload consumed as-is — exact key set and count
 * only, no state-word enumeration in the client (one vocabulary, the
 * server's; unknown state words render as text, never error). Same-origin
 * top-level /ready, never under /console and never /health. */
export function validateReadiness(v) {
  /* The retained contract fixes the SHAPE of these three keys; the #46
   * health rollup rides BESIDE them (lib.rs adds its fields without
   * touching these) and the strip consumes the payload as-is, so extra
   * top-level keys are expected — only the retained shapes are enforced. */
  const word = (s, max) => typeof s === 'string' && s.length <= max;
  if (v === null || typeof v !== 'object' || Array.isArray(v)
    || !word(v.status, 32) || !word(v.implementation, 32)
    || !Array.isArray(v.components) || v.components.length > 64
    || v.components.some((c) => !object(c, ['name', 'state']) || !word(c.name, 64) || !word(c.state, 64))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchReadiness(signal) {
  /* Top-level same-origin route (outside the console router); a 503 still
   * carries the payload, a network failure rejects — the strip's unknown. */
  const response = await fetch('/ready', { credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal });
  const value = await response.json();
  return validateReadiness(value);
}
export function resourceView(location) { return /^\/console\/resources\/?$/.test(location.pathname); }
export function selection(location, field = 'engagement_id') {
  const query = new URLSearchParams(location.search);
  if ([...query.keys()].some((k) => k !== field) || query.getAll(field).length > 1) throw new Error('invalid_selection');
  const value = query.get(field);
  if (value !== null && !id(value)) throw new Error('invalid_selection');
  return value;
}
export async function exchangeAccess(location, history, previousLogout = Promise.resolve()) {
  const fragment = location.hash;
  if (!fragment) return;
  history.replaceState(history.state, '', `${location.pathname}${location.search}`);
  if (!/^#access=[a-f0-9]{64}$/.test(fragment)) throw new Error('console_access_required');
  await previousLogout;
  await request('/session', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ticket: fragment.slice(8) }) });
  accessEnded = false;
}
/* `withReport` exists because two pages read this ONE list for different
 * purposes: the usage page needs the selected engagement's evidence, while the
 * engagements page is a triage list that never renders a report. The flag is
 * what stops the triage page issuing a usage read it then discards. */
export async function fetchNative(selected, after = '', withReport = true) {
  if ((selected !== null && !id(selected)) || (after && !id(after))) throw new Error('invalid_selection');
  const list = validateEngagements(await request(`/api/engagements?limit=16${after ? `&after=${after}` : ''}`));
  const chosen = selected ?? list.engagements[0]?.id ?? null;
  const report = !withReport || chosen === null ? null : validateReport(await request(`/api/engagements/${chosen}/usage`), chosen);
  // Every row the wire names is remembered, because no per-id route exists: a
  // selection the reader leaves the page with has no other way to keep its name.
  for (const e of list.engagements) remember(e.id, [e.agentName, e.projectName, e.role].filter(Boolean).join(' · '));
  return { ...list, selected: chosen, report };
}

/* The usage page's fleet panels: the totals block plus every side's
 * allocation/budget — the acceptance is "operator sets a side allocation
 * and sees spend vs allocation". Budget reads are one per side, capped at
 * 16 so a fleet registration burst cannot fan the page into an unbounded
 * request storm; sides beyond the cap render from the list without budget
 * figures. The engagement evidence itself stays on Data's own load. */
export async function fetchFleetUsage() {
  const [totals, sides] = await Promise.all([fetchUsageTotals(), fetchProjectSides()]);
  /* The key MUST be `budgets`: the panel spreads this object straight into its
   * state (`fleet.jsx`, `setState({ phase: 'ready', error: null, ...value })`)
   * and then indexes `state.budgets[side.id]`. Returning `sideBudgets` left
   * `state.budgets` undefined, so the spread rendered the fleet table and threw
   * `TypeError: Cannot read properties of undefined` — a crash no test saw
   * because the budget read always failed first and took the panel to its
   * error branch instead. Caught by the walk gate (board #108) once the read
   * succeeded. */
  const budgets = {};
  for (const side of sides.sides.slice(0, 16)) budgets[side.id] = await fetchSideBudget(side.id);
  return { totals, sides: sides.sides, budgets };
}

/* The usage page's load: the engagement evidence fetchNative returns,
 * beside the fleet panels. One combined reply so the page renders from one
 * snapshot, the way every other view does. */
export async function fetchUsageView(selected, after = '') {
  const [base, fleet] = await Promise.all([fetchNative(selected, after), fetchFleetUsage()]);
  return { ...base, ...fleet };
}
/* The triage document's own view test, beside its siblings: the engagements
 * read is selected from like the others, and the provider needs to tell it
 * apart from /usage to know whether a report is wanted at all. */
export function engagementsView(location) { return /^\/console\/engagements\/?$/.test(location.pathname); }
/* The console's open ceiling alerts. Exactly fifteen keys per alert — the
 * server's ConsoleAlert set — because the exact-key contract is how a stale
 * server or client fails loudly instead of rendering half a page. The
 * ENVELOPE additionally carries `permissions.configureResource` (brief 28):
 * the alerts read serves the session's capability the way the resources
 * read does, and the page hides the triage buttons without the configure
 * scope. `detail`
 * is the parsed payload object OR a truncated JSON string (the retained
 * truncatePayload rule, alert-store.js:61-64, ported at the store): the
 * object arm carries exactly the seven payload keys; the string arm accepts
 * any string and the page renders it as text — the same pass-through the
 * retained mapAlert does (mockup/lib/api.js:203).
 *
 * `severity === 'warning'` stays a hard equality check, so a future
 * non-warning alert makes the WHOLE READ throw invalid_native_response
 * rather than misrender — refuse, never silently relabel. `status` is the
 * STORE's real display-state column (ADR-124 amendment): one of the four
 * states from the one server-owned map, and every row carries `next`, the
 * transitions the SERVER allows from that state — the page renders buttons
 * ONLY from `next`, so a route the server does not serve can never appear
 * as a control and a client-side map can never disagree with the server's
 * (the retained console's own NEXT_STATUS drift is not ported). */
const DETAIL_KEYS = ['agent', 'presetId', 'ceilingTokens', 'committedTokens', 'measuredTokens', 'drawnTokens', 'overByTokens'];
const ALERT_KEYS = ['dedupe_key', 'resource_id', 'summary', 'detail', 'runbook', 'impact', 'recovery_condition', 'occurrences', 'first_seen_ms', 'last_seen_ms', 'resolved', 'severity', 'status', 'next', 'note'];
// Mirrors hagency_store::ALERT_STATUSES (the one server-owned map,
// domain/ceiling_alerts.rs:659 — the retained FIVE states, `assigned`
// restored): the validator must refuse an unknown state rather than
// misrender, so this list must move in the same commit as the store's.
// It had not: a served `next` naming `assigned` failed `validateAlerts`
// outright, so the whole alerts READ was refused as invalid_native_response
// and the page could only ever show its error panel.
const ALERT_STATUSES = ['open', 'acknowledged', 'assigned', 'resolved', 'suppressed'];
const validDetail = (v) => (v !== null && typeof v === 'object' && !Array.isArray(v)
  && Object.keys(v).length === DETAIL_KEYS.length && DETAIL_KEYS.every((k) => Object.hasOwn(v, k))
  && DETAIL_KEYS.every((k) => k === 'measuredTokens' ? (v[k] === null || number(v[k])) : (k === 'agent' || k === 'presetId' ? text(v[k], 256) : number(v[k]))))
  || text(v, 4096);
export function validateAlerts(v) {
  if (!object(v, ['at_ms', 'permissions', 'alerts']) || !number(v.at_ms)
    || !object(v.permissions, ['configureResource']) || typeof v.permissions.configureResource !== 'boolean'
    || !Array.isArray(v.alerts) || v.alerts.length > 200
    || v.alerts.some((a) => !object(a, ALERT_KEYS)
      || !text(a.dedupe_key, 256) || !id(a.resource_id) || !text(a.summary, 2048)
      || !validDetail(a.detail) || !text(a.runbook, 2048) || !text(a.impact, 2048) || !text(a.recovery_condition, 2048)
      || !number(a.occurrences) || !number(a.first_seen_ms) || !number(a.last_seen_ms)
      || typeof a.resolved !== 'boolean' || a.severity !== 'warning'
      || !ALERT_STATUSES.includes(a.status)
      || !Array.isArray(a.next) || a.next.length > 4 || a.next.some((s) => !ALERT_STATUSES.includes(s))
      || (a.note !== null && !text(a.note, 2048)))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAlerts() {
  return validateAlerts(await request('/api/alerts?limit=100'));
}
export function alertsView(location) { return /^\/console\/alerts\/?$/.test(location.pathname); }

/* The agent roster (ADR-126, widened by board #22): one row per AGENT —
 * the TS roster's shape, every agent the service knows — with EXACTLY
 * nine scalar keys, the server's RosterItem set, and the same exact-key
 * contract every other native read carries: an added server key (a
 * tmux target, a workspace path) fails the whole read rather than
 * rendering. No nested object exists on a roster item, so nothing can
 * hide inside one. `online` is REAL worker state (a live dispatch in
 * one of the agent's sessions); `last_seen_ms` is the newest attempt
 * clock the agent produced — null, never zero, when it never attempted;
 * `last_activity_ms` keeps the representative engagement's attempt
 * clock. `unavailable` is SERVER-OWNED: whatever columns the server
 * names are rendered as unknown, so a future source turns a column on
 * by removing its name server-side, never by a client edit. Board #60:
 * `liveness` (the live dispatch's own word, distinct from the engagement
 * `state`) and `consumed` (observed tokens, null when unmeasured) are
 * served now, so this list is empty. */
const ROSTER_KEYS = ['name', 'framework', 'role', 'state', 'engagement_id', 'requested_tokens', 'online', 'last_seen_ms', 'last_activity_ms', 'liveness', 'consumed', 'quota_paused'];
export function validateAgents(v) {
  if (!object(v, ['at_ms', 'unavailable', 'agents', 'permissions']) || !number(v.at_ms)
    || !Array.isArray(v.unavailable) || v.unavailable.length > 32 || v.unavailable.some((n) => !text(n, 64))
    || !Array.isArray(v.agents) || v.agents.length > 100
    || !object(v.permissions, ['manageLifecycle']) || typeof v.permissions.manageLifecycle !== 'boolean'
    || v.agents.some((a) => !object(a, ROSTER_KEYS)
      || !text(a.name, 128) || !text(a.framework, 64) || !text(a.role, 128)
      || !STATES.includes(a.state) || !id(a.engagement_id)
      || !number(a.requested_tokens) || typeof a.online !== 'boolean'
      || !(a.last_seen_ms === null || number(a.last_seen_ms))
      || !(a.last_activity_ms === null || number(a.last_activity_ms))
      || !(a.liveness === null || text(a.liveness, 32))
      || !(a.consumed === null || number(a.consumed))
      || typeof a.quota_paused !== 'boolean')) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAgents() {
  return validateAgents(await request('/api/agents'));
}
export function agentsView(location) { return /^\/console\/agents\/?$/.test(location.pathname); }

/* The agent detail (board #22, TS backend-v2.js:12155): the agent-keyed
 * identity plus the resource it works from, the rooms its sessions bind,
 * its current live dispatch and its recent tasks — EXACTLY the declared
 * keys, same fail-closed contract: a widened server payload fails the
 * whole read. `dispatch` is null when nothing is live; `tasks` carries
 * the ten most recently touched canonical tasks, newest first. The agent
 * name comes from the URL, not the payload, so a served record naming a
 * DIFFERENT agent also fails the read. */
const ROOM_KEYS = ['session_id', 'room_id', 'dispatch_state', 'dispatch_id'];
const TASK_STATES = ['created', 'accepted', 'in_progress', 'blocked', 'done'];
const REMINDER_KEYS = ['id', 'engagement_id', 'session_id', 'msg', 'created_at', 'fire_at', 'fired_at'];
const detailRoom = (v) => v !== null && object(v, ROOM_KEYS) && id(v.session_id) && text(v.room_id, 256)
  && (v.dispatch_state === null || ['queued', 'leased', 'started', 'parked', 'completed', 'outcome_unknown', 'superseded'].includes(v.dispatch_state))
  && (v.dispatch_id === null || id(v.dispatch_id));
const detailReminder = (v) => v !== null && object(v, REMINDER_KEYS) && number(v.id) && id(v.engagement_id)
  && id(v.session_id) && text(v.msg, 32768) && number(v.created_at) && number(v.fire_at)
  && (v.fired_at === null || number(v.fired_at));
export function validateAgentDetail(v, name) {
  if (!object(v, ['name', 'framework', 'role', 'state', 'engagement_id', 'requested_tokens', 'online', 'last_seen_ms', 'resource_id', 'project_id', 'engagements', 'rooms', 'dispatch', 'tasks', 'reminders'])
    || v.name !== name || !text(v.name, 128) || !text(v.framework, 64) || !text(v.role, 128)
    || !STATES.includes(v.state) || !id(v.engagement_id) || !number(v.requested_tokens)
    || typeof v.online !== 'boolean' || !(v.last_seen_ms === null || number(v.last_seen_ms))
    || !id(v.resource_id) || !id(v.project_id) || !number(v.engagements)
    || !Array.isArray(v.rooms) || v.rooms.length > 100 || v.rooms.some((r) => !detailRoom(r))
    // `dispatch` is null when nothing is live (domain.rs picks only
    // leased/started/parked rooms; a stopped dispatch is outcome_unknown and
    // yields none) — the read's own contract, not a malformed payload.
    || !(v.dispatch === null || detailRoom(v.dispatch))
    || !Array.isArray(v.tasks) || v.tasks.length > 10
    || v.tasks.some((task) => !object(task, ['id', 'session_id', 'creator_session_id', 'title', 'description', 'priority', 'granularity', 'labels', 'parent_id', 'status', 'execution_epoch', 'created_at', 'updated_at', 'started_at', 'completed_at', 'heartbeat_at', 'waiting_reason', 'waiting_until'])
      || !id(task.id) || !id(task.session_id) || !text(task.title, 1024) || !TASK_STATES.includes(task.status)
      || !number(task.execution_epoch) || !number(task.created_at) || !number(task.updated_at))
    || !Array.isArray(v.reminders) || v.reminders.length > 100 || v.reminders.some((r) => !detailReminder(r))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAgentDetail(name) {
  return validateAgentDetail(await request(`/api/agents/${encodeURIComponent(name)}`), name);
}

/* The launch runtime profile (board #49, TS backend-v2.js:12344
 * GET /api/agents/:name/launch-env). Exactly the TS envelope `{runtimeProfile}`
 * and, inside it, the `normalizeRuntimeProfile` shape `{primary, supervisor}`
 * (backend-v2.js:864-876). `supervisor` is null when the record carries none,
 * which is always here — the port stores no supervisor profile, and null is
 * rendered as unknown, never invented. A role carries framework/provider/model/
 * reasoning only: extraArgs, apiBaseUrl and apiKey have no native source and
 * are refused by exact-key checking rather than silently dropped. */
const runtimeRole = (v) => object(v, ['framework', 'provider', 'model', 'reasoning'])
  && text(v.framework, 64) && text(v.model, 256)
  && (v.provider === null || text(v.provider, 64))
  && (v.reasoning === null || text(v.reasoning, 64));
export function validateLaunchEnv(v) {
  if (!object(v, ['runtimeProfile'])) throw new Error('invalid_native_response');
  const p = v.runtimeProfile;
  if (p !== null && (!object(p, ['primary', 'supervisor'])
    || (p.primary !== null && !runtimeRole(p.primary))
    || (p.supervisor !== null && !runtimeRole(p.supervisor)))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAgentLaunchEnv(name) {
  return validateLaunchEnv(await request(`/api/agents/${encodeURIComponent(name)}/launch-env`));
}

/* Undelete an agent (board #49, TS backend-v2.js:12308-12316). The retained
 * route answers exactly `{ok, undeleted, name}` when a force-delete
 * tombstone exists — the key a caller must check is `undeleted`, not `ok`,
 * because the 404 arm is the interesting outcome here: it means no
 * tombstone stood, so there was nothing to reverse. */
export async function undeleteAgent(name) {
  const v = await request(`/api/agents/${encodeURIComponent(name)}/undelete`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({}) });
  if (!object(v, ['ok', 'undeleted', 'name']) || v.ok !== true || v.undeleted !== true || v.name !== name) throw new Error('invalid_native_response');
  return v;
}

/* Queue an avatar request (board #49, TS backend-v2.js:16370-16377). TS
 * handed the request to the bridge over SSE and answered immediately
 * `{ok, queued, name, force, custom}` — the response is a receipt, never
 * the avatar. `force` is body.generate OR ?force=true; `custom` is whether
 * an image rode the request. The base64 payload is capped where the
 * retained express.json limit capped it (10mb). */
export async function requestAgentAvatar(name, { generate = false, image = null, mime = null, force = false } = {}) {
  const body = {};
  if (generate) body.generate = true;
  if (image !== null) body.image = image;
  if (mime !== null) body.mime = mime;
  const path = force ? `/api/agents/${encodeURIComponent(name)}/avatar?force=true` : `/api/agents/${encodeURIComponent(name)}/avatar`;
  const v = await request(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }, 10 * 1024 * 1024 + 4096);
  if (!object(v, ['ok', 'queued', 'name', 'force', 'custom'])
    || v.ok !== true || v.queued !== true || v.name !== name
    || typeof v.force !== 'boolean' || typeof v.custom !== 'boolean') throw new Error('invalid_native_response');
  return v;
}

/* The agent's delivery events (board #49, TS backend-v2.js:16988-17000):
 * `{agent, events}` newest-first. An event carries the retained row shape
 * (id, messageId?, type, agent, source?, reason?, context?, ts) — optional
 * keys are ABSENT when unset (serde skip_serializing_if), so the validator
 * checks presence only when the key exists. `agent` must name the agent
 * asked for, exactly as `validateAgentDetail` pins its own name. */
const deliveryEvent = (v) => object(v, ['id', 'type', 'agent', 'ts'])
  && number(v.id) && number(v.ts) && text(v.type, 64) && text(v.agent, 128)
  && (!Object.hasOwn(v, 'messageId') || text(v.messageId, 128))
  && (!Object.hasOwn(v, 'source') || text(v.source, 64))
  && (!Object.hasOwn(v, 'reason') || text(v.reason, 255))
  && (!Object.hasOwn(v, 'context') || (v.context !== null && typeof v.context === 'object' && !Array.isArray(v.context)));
export function validateDeliveryEvents(v, name) {
  if (!object(v, ['agent', 'events']) || v.agent !== name || !Array.isArray(v.events) || v.events.length > 1000
    || v.events.some((e) => !deliveryEvent(e))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAgentDeliveryEvents(name, limit = null) {
  const suffix = limit === null ? '' : `?limit=${encodeURIComponent(limit)}`;
  return validateDeliveryEvents(await request(`/api/agents/${encodeURIComponent(name)}/delivery-events${suffix}`), name);
}

/* CL-S2 (ADR-130): expose only the lifecycle mutation that has a durable
 * domain effect. Start and preset rebinding fail closed at the server until
 * their complete state transitions exist; the browser must not offer buttons
 * that can only refuse. */
/* #21 lifecycle mutations (TS parity: backend-v2.js:12577-12775 stop/start,
 * :11484-11522 preset). The browser offers only what the server does:
 * stop completes, start re-arms serving, preset rebinds the resource the
 * next dispatch claims. Every outcome answer is validated exact-key. */
export async function stopAgent(id) {
  return request(`/api/agents/${id}/stop`, { method: 'POST' });
}
export async function startAgent(id) {
  const value = await request(`/api/agents/${id}/start`, { method: 'POST' });
  if (!object(value, ['ok', 'state']) || value.ok !== true || value.state !== 'launching') throw new Error('invalid_native_response');
  return value;
}
export async function rebindAgentResource(id, presetId) {
  const value = await request(`/api/agents/${id}/preset`, { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ presetId }) });
  if (!object(value, ['ok', 'engagement_id', 'preset_id', 'resource_id', 'previous_preset_id', 'ceiling_tokens', 'remaining', 'tier'])
    || value.ok !== true || value.engagement_id !== id || !text(value.preset_id, 128) || !text(value.resource_id, 128)
    || !(value.ceiling_tokens === null || number(value.ceiling_tokens)) || !(value.remaining === null || number(value.remaining))) throw new Error('invalid_native_response');
  return value;
}

/* Remove an agent (board #58, TS backend-v2.js:12164). The two shapes are
 * kept APART on purpose, because the retained route's own trap is a caller
 * that checks only `ok`: a plain DELETE answers `{ok:true, deprecated:true,
 * message}` and leaves the agent in place, while `?force=true` really
 * removes it and reports what it released. So `force` decides which key must
 * be present, and a response that says `ok` without it is refused as
 * invalid rather than reported as a successful removal. */
export async function deleteAgent(name, force = false) {
  const v = await request(`/api/agents/${encodeURIComponent(name)}${force ? '?force=true' : ''}`, { method: 'DELETE' });
  if (v?.ok !== true) throw new Error('invalid_native_response');
  if (force) {
    if (v.deleted !== true || typeof v.sessionKilled !== 'boolean'
      || !Array.isArray(v.releasedEngagements) || v.releasedEngagements.some((id_) => !id(id_))
      || !Array.isArray(v.leftGroups) || !Array.isArray(v.leftProjectRooms)) throw new Error('invalid_native_response');
  } else if (v.deprecated !== true || typeof v.message !== 'string') {
    throw new Error('invalid_native_response');
  }
  return v;
}

/* The project-sides read (ADR-132): one row per fleet registration — the
 * id IS the server name (ADR-016) — with EXACTLY six keys and projects
 * entries of exactly {id, room_id}. The exact-key contract is the privacy
 * guard: no credential key exists in either set, so a server that grew
 * one (as_token, hs_token, anything credential-shaped) fails the whole
 * read rather than rendering. `owner` fields are absent by design
 * (ADR-112); the unavailable list is SERVER-OWNED and rendered verbatim —
 * unknown is never zero and never invented. `registered` is "this
 * registration row exists at the fleet's current generation", not an
 * access verdict. */
const SIDE_KEYS = ['id', 'representative', 'generation', 'registered', 'reception_room_id', 'projects'];
const room = (v) => typeof v === 'string' && v.length <= 256;
export function validateProjectSides(v) {
  if (!object(v, ['at_ms', 'unavailable', 'sides']) || !number(v.at_ms)
    || !Array.isArray(v.unavailable) || v.unavailable.length > 32 || v.unavailable.some((n) => !text(n, 64))
    || !Array.isArray(v.sides) || v.sides.length > 1024
    || v.sides.some((s) => !object(s, SIDE_KEYS)
      || !text(s.id, 255) || !text(s.representative, 255) || !number(s.generation)
      || typeof s.registered !== 'boolean' || !room(s.reception_room_id)
      || !Array.isArray(s.projects) || s.projects.length > 64
      || s.projects.some((p) => !object(p, ['id', 'room_id']) || !text(p.id, 128) || !room(p.room_id)))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchProjectSides() {
  return validateProjectSides(await request('/api/project-sides'));
}
export function projectSidesView(location) { return /^\/console\/project-sides\/?$/.test(location.pathname); }
export function usageView(location) { return /^\/console\/usage\/?$/.test(location.pathname); }
/* The side lifecycle mutations (board #14). Each reply is the same
 * `{ok:true, side}` envelope the single-side read serves, so one validator
 * governs every write AND the read — the page never re-derives a side. The
 * credential VALUE is write-only (ADR-016 decision 8): no fetch returns it,
 * and the projection carries only `credentialKind`/`hasCredential`, so a
 * token can never reach the browser's memory from a read. */
const SIDE_LIFECYCLE_KEYS = ['id', 'label', 'serverName', 'apiBaseUrl', 'credentialKind', 'hasCredential', 'awaitingInstall', 'awaitingInstallSince', 'senderLocalpart', 'appserviceUrl', 'namespace', 'accessIssuedAt', 'accessState', 'accessDetail', 'accessCheckedAt', 'allocatedTokens', 'representative', 'projects', 'active', 'createdAt', 'updatedAt'];
const projectRow = (p) => object(p, ['id', 'name', 'roomId', 'note', 'archived', 'archivedAt', 'createdAt', 'updatedAt'])
  && text(p.id, 128) && text(p.name, 255)
  && (p.roomId === null || text(p.roomId, 256))
  && (p.note === null || text(p.note, 1024))
  && typeof p.archived === 'boolean' && (p.archivedAt === null || number(p.archivedAt))
  && number(p.createdAt) && number(p.updatedAt);
const validSideRecord = (s) => object(s, SIDE_LIFECYCLE_KEYS)
  && text(s.id, 255) && text(s.label, 255) && text(s.serverName, 255)
  && (s.apiBaseUrl === null || text(s.apiBaseUrl, 1024))
  && (s.credentialKind === null || ['appservice', 'registrationToken'].includes(s.credentialKind))
  && typeof s.hasCredential === 'boolean' && typeof s.awaitingInstall === 'boolean'
  && (s.awaitingInstallSince === null || number(s.awaitingInstallSince))
  && (s.senderLocalpart === null || text(s.senderLocalpart, 255))
  && (s.appserviceUrl === null || text(s.appserviceUrl, 1024))
  && (s.namespace === null || text(s.namespace, 255))
  && (s.accessIssuedAt === null || number(s.accessIssuedAt))
  && ['unverified', 'accepted', 'rejected', 'unreachable', 'blocked'].includes(s.accessState)
  && (s.accessDetail === null || text(s.accessDetail, 1024))
  && (s.accessCheckedAt === null || number(s.accessCheckedAt))
  && (s.allocatedTokens === null || number(s.allocatedTokens))
  && (s.representative === null || (object(s.representative, ['mxid', 'localpart', 'observedAt']) && text(s.representative.mxid, 255) && text(s.representative.localpart, 255) && number(s.representative.observedAt)))
  && Array.isArray(s.projects) && s.projects.every(projectRow)
  && typeof s.active === 'boolean' && number(s.createdAt) && number(s.updatedAt);
function validSideEnvelope(v) {
  if (!object(v, ['ok', 'side']) || v.ok !== true || !validSideRecord(v.side)) throw new Error('invalid_native_response');
  return v.side;
}
export async function fetchSide(id) {
  return validSideEnvelope(await request(`/api/project-sides/${encodeURIComponent(id)}`));
}
export async function setSideCredential(id, credential) {
  return validSideEnvelope(await request(`/api/project-sides/${encodeURIComponent(id)}/credential`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ credential }),
  }));
}
export async function verifySide(id) {
  /* The verdict rides on the returned side; the `promoted` fact is the
   * operator-visible "did my staged credential take effect". */
  return request(`/api/project-sides/${encodeURIComponent(id)}/verify`, { method: 'POST' });
}
export async function addSideProject(id, input) {
  return request(`/api/project-sides/${encodeURIComponent(id)}/projects`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(input),
  });
}
export async function archiveSideProject(id, projectId, archived = true) {
  return request(`/api/project-sides/${encodeURIComponent(id)}/projects/${encodeURIComponent(projectId)}/archive`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ archived }),
  });
}
export async function deactivateSide(id) {
  return validSideEnvelope(await request(`/api/project-sides/${encodeURIComponent(id)}/deactivate`, { method: 'POST' }));
}
export async function reactivateSide(id) {
  return validSideEnvelope(await request(`/api/project-sides/${encodeURIComponent(id)}/reactivate`, { method: 'POST' }));
}
export async function removeSide(id, force = false) {
  return request(`/api/project-sides/${encodeURIComponent(id)}${force ? '?force=true' : ''}`, { method: 'DELETE' });
}
/* The bounded decision receipt every engagement mutation answers — `id`,
 * `state`, `cleanup` and nothing else (engagements.rs `receipt()`). One
 * validator for approve, refuse's sibling reads and the retire pair,
 * because they are one route shape. */
export function validateEngagementReceipt(v) {
  if (!object(v, ['id', 'state', 'cleanup']) || !id(v.id) || !STATES.includes(v.state) || !CLEANUP.includes(v.cleanup)) throw new Error('invalid_native_response');
  return v;
}
/* Approve / refuse a pending engagement (board #16). Approve reaches the
 * SAME store verdict the project-side Matrix approval reaches; refuse rides
 * the existing /api/agents/{id}/refuse route and answers the rejected
 * engagement. The command id is minted client-side: it is the store's
 * idempotency key, never the route's. */
export async function approveEngagement(engagementId, commandId, allocatedTokens = null) {
  const body = allocatedTokens === null ? { commandId } : { commandId, allocatedTokens };
  return validateEngagementReceipt(await request(`/api/engagements/${encodeURIComponent(engagementId)}/approve`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }));
}
export async function refuseEngagement(engagementId, commandId) {
  const v = await request(`/api/agents/${encodeURIComponent(engagementId)}/refuse`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ commandId }) });
  if (!object(v, ['engagement']) || !id(v.engagement?.id) || !STATES.includes(v.engagement?.state)) throw new Error('invalid_native_response');
  return v.engagement;
}
/* End an engagement (ADR-150) and retry its failed cleanup. The worker
 * replaces itself with a payment, the entitlement stops metering, and the
 * initial grant is reclaimed; a failed retirement waits for this operator
 * act — native has no sweeper or timer (engagements.rs:12). */
export async function retireEngagement(engagementId, commandId) {
  return validateEngagementReceipt(await request(`/api/engagements/${encodeURIComponent(engagementId)}/retire`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ commandId }) }));
}
function validateSettlement(v) {
  if (!v || !['awaiting_final_usage', 'settled', 'late_usage_charged'].includes(v.state)
    || !id(v.agentAllocationId) || !id(v.resourceAllocationId) || !Number.isSafeInteger(v.allocatedTokens)
    || typeof v.period !== 'string' || typeof v.periodKey !== 'string'
    || (v.state !== 'awaiting_final_usage' && (!Number.isSafeInteger(v.consumedTokens) || !Number.isSafeInteger(v.releasedTokens)))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchAgentSettlement(engagementId) {
  return validateSettlement(await request(`/api/engagements/${encodeURIComponent(engagementId)}/settlement`));
}
export async function settleAgentUsage(body) {
  return validateSettlement(await request(`/api/engagements/${encodeURIComponent(body.agentAllocationId)}/settlement`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  }));
}
/* ADR-186 §C: add tokens to a reserved or active engagement's allocation.
 * Checked by the store like an approval; a refusal carries the store's
 * explanation as `error.detail`. The answer is the bounded receipt. */
export async function raiseEngagementAllocation(engagementId, commandId, addTokens) {
  return validateEngagementReceipt(await request(`/api/engagements/${encodeURIComponent(engagementId)}/allocation`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ commandId, addTokens }) }));
}
export async function retryEngagementCleanup(engagementId, commandId) {
  return validateEngagementReceipt(await request(`/api/engagements/${encodeURIComponent(engagementId)}/cleanup-retry`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ commandId }) }));
}
/* Register a fleet side (G11): the retained POST /api/project-sides shape on
 * the console API, behind the console session. The answer is the
 * saved record's own five fields, never the operator token. */
export async function registerProjectSide(registration) {
  const v = await request('/api/project-sides', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(registration) });
  if (!object(v, ['ok', 'side']) || v.ok !== true
    || !object(v.side, ['id', 'generation', 'server_name', 'reception_room_id', 'representative'])
    || typeof v.side.id !== 'string' || !number(v.side.generation)) throw new Error('invalid_native_response');
  return v.side;
}
export async function transitionAlert(key, to, note) {
  /* One display-state transition through the console session. The reply is
   * the SAME envelope the list read serves (one row), so the same validator
   * governs it — `next` arrives from the server, never re-derived here. */
  return validateAlerts(await request(`/api/alerts/${encodeURIComponent(key)}/transition`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ to, ...(note ? { note } : {}) }),
  }));
}
export async function logoutNative() { accessEnded = true; await request('/session', { method: 'DELETE' }); }

/* The read-only approval observation (ADR-138, PC-C2b): the list and single
 * routes serve rows with exactly seven camelCase keys and no nested object,
 * so the exact-key helper (set + count) is what keeps a widened server
 * projection from rendering half a row. `state` is one of the seven CHECK
 * words, `choice` one of the four snake_case words or null. A row can never
 * carry a card or a preview: there is no key for one. */
const APPROVAL_STATES = ['pending', 'decided', 'applying', 'uncertain', 'applied', 'invalidated', 'not_applied'];
const APPROVAL_CHOICES = ['once', 'task', 'always', 'deny'];
const approvalRow = (a) => object(a, ['id', 'state', 'choice', 'reusableScope', 'expiresAt', 'engagementId', 'projectRoomId'])
  && id(a.id) && APPROVAL_STATES.includes(a.state)
  && (a.choice === null || APPROVAL_CHOICES.includes(a.choice))
  && typeof a.reusableScope === 'boolean' && number(a.expiresAt)
  && id(a.engagementId) && (a.projectRoomId === null || text(a.projectRoomId, 256));
export function validateApprovals(v) {
  if (!object(v, ['approvals', 'next_after']) || !Array.isArray(v.approvals) || v.approvals.length > 16
    || !(v.next_after === null || id(v.next_after))
    || v.approvals.some((a) => !approvalRow(a))) throw new Error('invalid_native_response');
  return v;
}
export function validateApproval(v) {
  if (!approvalRow(v)) throw new Error('invalid_native_response');
  return v;
}
export async function fetchApprovals(after = '') {
  if (after && !id(after)) throw new Error('invalid_selection');
  return validateApprovals(await request(`/api/approvals?limit=16${after ? `&after=${after}` : ''}`));
}
export async function fetchApproval(key) {
  if (!id(key)) throw new Error('invalid_selection');
  return validateApproval(await request(`/api/approvals/${encodeURIComponent(key)}`));
}
export function approvalsView(location) { return /^\/console\/approvals\/?$/.test(location.pathname); }

/*
 * The read-only approval-bindings list (board #52, TS `GET
 * /api/approval-bindings` at backend-v2.js:9051-9082, the plain-list branch):
 * exactly the nine keys the route serves, and nothing else — no
 * `agentJoined`, no `active`, no authority id, because native derives a
 * binding from its own room observations and none of those columns exist to
 * serve. An extra key fails the whole read, the same discipline the
 * approvals projection applies.
 */
const bindingRow = (v) => object(v, ['engagementId', 'agent', 'fleetId', 'projectId', 'serverName', 'roomId', 'ownerMxid', 'roomGeneration', 'incarnation'])
  && id(v.engagementId) && text(v.agent, 128) && id(v.fleetId) && id(v.projectId)
  && text(v.serverName, 256) && text(v.roomId, 256) && text(v.ownerMxid, 256)
  && number(v.roomGeneration) && number(v.incarnation);
export function validateApprovalBindings(v) {
  if (!object(v, ['at_ms', 'bindings']) || !Array.isArray(v.bindings) || v.bindings.length > 100
    || v.bindings.some((b) => !bindingRow(b))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchApprovalBindings() {
  return validateApprovalBindings(await request('/api/approval-bindings'));
}

/*
 * The operator unbind (board #52, TS `DELETE
 * /api/approval-bindings/:agent/:roomId` at backend-v2.js:9032-9046). integ
 * is SINGLE-LOGIN (operator decision): any authenticated session may unbind —
 * no scope word, no capability word — while an anonymous caller is refused
 * 401 before the route. Both ids are percent-encoded path segments; the
 * server validates the room shape, so a malformed value is a refusal, never a
 * silent success.
 */
export async function unbindApprovalBinding(agent, roomId) {
  const v = await request(`/api/approval-bindings/${encodeURIComponent(agent)}/${encodeURIComponent(roomId)}`, { method: 'DELETE' });
  if (!object(v, ['ok', 'binding']) || v.ok !== true || !bindingRow(v.binding)) throw new Error('invalid_native_response');
  return v.binding;
}

/* The fleet usage totals (backend-v2.js:15700-15720): the numerator never
 * travels without its denominator, and null means "not known", never zero.
 * busySec and tasks are named in the server-owned unavailable list — the
 * retained block sums them from the agents' busy clocks and the task
 * store, which native has no source for; unknown is never zero. */
export function validateUsageTotals(v) {
  if (!object(v, ['ok', 'totals', 'unavailable']) || v.ok !== true
    || !Array.isArray(v.unavailable) || v.unavailable.length > 8 || v.unavailable.some((n) => !text(n, 64))
    || !object(v.totals, ['agents', 'tokensDrawn', 'tokensUsed', 'tokensMeasuredFor', 'tokensPartial'])
    || !number(v.totals.agents) || !number(v.totals.tokensMeasuredFor)
    || !(v.totals.tokensDrawn === null || number(v.totals.tokensDrawn))
    || !(v.totals.tokensUsed === null || number(v.totals.tokensUsed))
    || typeof v.totals.tokensPartial !== 'boolean') throw new Error('invalid_native_response');
  return v;
}
export async function fetchUsageTotals() { return validateUsageTotals(await request('/api/usage/totals')); }

/* The side budget (backend-v2.js:9254-9307, 9541, 9567). A side id is a
 * MATRIX SERVER NAME — the retained store normalizes it through
 * SERVER_NAME_RE (lib/project-side-store.js:48) — lowercase, dots and a
 * possible port, which the console's own id() charset refuses by design.
 * NULL allocated is unallocated, not unlimited; remaining is null with it. */
const serverName = (v) => typeof v === 'string' && v.length <= 255 && /^[a-z0-9][a-z0-9.\-]*(:\d{1,5})?$/.test(v);
const commitment = (v) => object(v, ['id', 'agent', 'role', 'project', 'projectName', 'allocatedTokens', 'agentExists'])
  && id(v.id) && text(v.agent, 128) && text(v.role, 128) && id(v.project)
  && optionalScalarText(v.projectName, 255) && number(v.allocatedTokens) && typeof v.agentExists === 'boolean';
const BUDGET_KEYS = ['allocated', 'committed', 'remaining', 'commitments', 'poolCommitments', 'poolCommitted', 'totalCommitted', 'orphanedCommitted'];
/* The budget VALUES, independent of the object's key set. The SAME eight
 * fields are served two ways: nested (PUT `{ok, side, budget}`) and spread
 * FLAT beside `ok`/`sideId` (GET — the retained route's own spread,
 * backend-v2.js:9571, which this module's own comment above records). The
 * value check must therefore not demand an exact key count: requiring
 * exactly eight keys made EVERY budget GET fail as
 * `invalid_native_response`, so `fetchSideBudget` always threw, the fleet
 * panel's read never resolved, and the page could only ever render
 * "Fleet usage could not be read". */
const budgetValues = (b) => (b.allocated === null || number(b.allocated)) && number(b.committed)
  && (b.remaining === null || number(b.remaining))
  && Array.isArray(b.commitments) && b.commitments.length <= 1024 && b.commitments.every(commitment)
  && Array.isArray(b.poolCommitments) && b.poolCommitments.length <= 1024 && b.poolCommitments.every(commitment)
  && number(b.poolCommitted) && number(b.totalCommitted) && number(b.orphanedCommitted);
/* The nested form pins its exact eight-key set; the flat form's exact set is
 * pinned by `validateSideBudget`'s own ten-key top-level check. */
const budgetFields = (b) => object(b, BUDGET_KEYS) && budgetValues(b);
export function validateSideBudget(v, side) {
  if (!object(v, ['ok', 'sideId', ...BUDGET_KEYS])
    || v.ok !== true || v.sideId !== side || !budgetValues(v)) throw new Error('invalid_native_response');
  return v;
}
/* PUT replies {ok, side, budget} — the SAME six-key side record the list
 * read serves and the SAME budget the GET spreads flat. */
export function validateAllocationReply(v) {
  if (!object(v, ['ok', 'side', 'budget']) || v.ok !== true
    || !object(v.side, SIDE_KEYS) || !text(v.side.id, 255) || !text(v.side.representative, 255)
    || !number(v.side.generation) || typeof v.side.registered !== 'boolean' || !room(v.side.reception_room_id)
    /* The per-side cap is 64 (the store's own ROW_NUMBER window, domain.rs),
     * and the list read above bounds it the same way. This bound was INVERTED
     * — `<= 64` — so it rejected every legal side (0..=64 projects) and
     * accepted only an illegal one, which made `setSideAllocation` throw on
     * every save and the panel's write path unreachable. Found by the new
     * flat/nested budget test below. */
    || !Array.isArray(v.side.projects) || v.side.projects.length > 64
    || !v.side.projects.every((p) => object(p, ['id', 'room_id']) && text(p.id, 128) && room(p.room_id))
    || !budgetFields(v.budget)) throw new Error('invalid_native_response');
  return v;
}
export async function fetchSideBudget(side) {
  if (!serverName(side)) throw new Error('invalid_selection');
  return validateSideBudget(await request(`/api/project-sides/${encodeURIComponent(side)}/budget`), side);
}
export async function setSideAllocation(side, allocatedTokens) {
  if (!serverName(side) || !(allocatedTokens === null || number(allocatedTokens))) throw new Error('invalid_selection');
  return validateAllocationReply(await request(`/api/project-sides/${encodeURIComponent(side)}/allocation`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ allocated_tokens: allocatedTokens }),
  }));
}

const revision = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
const text = (v, max) => typeof v === 'string' && v.length <= max;
const optionalText = (v, max) => v === null || text(v, max);
/* projectName is bounded in Unicode SCALAR VALUES (code points), like
 * `validateEngagements` above and the server's own `trim().chars().take(255)`
 * (authority.rs:286): 255 astral characters are 510 UTF-16 units, so a
 * `.length` bound refuses a read the server legitimately serves. The side
 * budget's commitments are the OTHER place a project name travels, and they
 * used the UTF-16 bound — so ONE such name (the live fleet's AlertWorker,
 * "𝕏".repeat(260) truncated to 255 scalars) refused every budget read, which
 * took the whole fleet panel down with it ("Fleet usage could not be read"). */
const optionalScalarText = (v, max) => v === null || (typeof v === 'string' && [...v].length <= max);
const periodFields = (v) => !Object.hasOwn(v, 'period') || v.period === null || text(v.period, 64 * 1024);
const ceiling = (v) => v === null || (v && Object.keys(v).every((k) => ['tokens', 'period'].includes(k)) && (v.tokens === null || number(v.tokens)) && periodFields(v));
const validResource = (r) => !(!object(r, ['id', 'framework', 'model', 'provider', 'reasoning', 'ceiling', 'published', 'roles', 'revision'])
      || !id(r.id) || !text(r.framework, 64) || !text(r.model, 256) || !optionalText(r.provider, 128) || !optionalText(r.reasoning, 128)
      || !ceiling(r.ceiling) || typeof r.published !== 'boolean' || !Array.isArray(r.roles) || r.roles.length > 64 || !r.roles.every((v) => text(v, 64)) || !revision(r.revision));
export function validateResources(value) {
  if (!object(value, ['resources', 'roles', 'next_after', 'permissions']) || !Array.isArray(value.resources) || value.resources.length > 16
    || !(value.next_after === null || id(value.next_after)) || !object(value.permissions, ['publishResource', 'configureResource']) || typeof value.permissions.publishResource !== 'boolean' || typeof value.permissions.configureResource !== 'boolean'
    || value.resources.some((r) => !validResource(r))
    || !Array.isArray(value.roles) || value.roles.length !== 6 || value.roles.some((r) => !object(r, ['role', 'explicitPublication', 'available', 'crossFamily', 'defaultTier', 'families', 'fillable', 'overTier'])
      || !text(r.role, 64) || !(r.explicitPublication === null || typeof r.explicitPublication === 'boolean') || typeof r.available !== 'boolean' || typeof r.crossFamily !== 'boolean'
      || !(r.defaultTier === null || ['lightweight', 'medium', 'strong'].includes(r.defaultTier))
      || !Array.isArray(r.families) || r.families.length > 8 || r.families.some((f) => !text(f, 64))
      || !number(r.fillable) || r.fillable > 1024 || !number(r.overTier) || r.overTier > 1024)) throw new Error('invalid_native_response');
  return value;
}
export function validateBudget(v) {
  const amount = (n) => n === null || number(n);
  /* The `draw` object (brief 18) carries the headroom figures the page
   * renders. Unknown is null, never zero: `measured`/`consumed` are null
   * when the period is unmeasured, `ceilingTokens`/`remainingBeforeCeiling`
   * are null when no ceiling is declared, and `binding` is null when the
   * measurement is unknown (the commitment stands alone — nothing competes
   * for the ceiling, so nothing binds it). `binding` otherwise names the
   * binding draw the way ADR-122's refusal does (engagement-store.js:82-83:
   * measured > committed ? 'measured spend' : 'committed allocations').
   * This key set is exact in BOTH directions with the route's serializer. */
  const binding = (b) => b === null || b === 'measured spend' || b === 'committed allocations';
  const draw = (d) => d !== null && typeof d === 'object' && !Array.isArray(d)
    && Object.keys(d).length === 8
    && ['committed', 'measured', 'consumed', 'drawn', 'ceilingTokens', 'period', 'binding', 'remainingBeforeCeiling'].every((k) => Object.hasOwn(d, k))
    && number(d.committed) && amount(d.measured) && amount(d.consumed) && number(d.drawn)
    && amount(d.ceilingTokens) && ['daily', 'monthly'].includes(d.period) && binding(d.binding)
    && amount(d.remainingBeforeCeiling);
  /* Brief 20 (E3): the top-level `reserved` key is GONE from the console
   * wire — it was a constant 0 there (`budget()` runs with no exclusion;
   * the core assigns `reserved` only inside the exclude arm,
   * allocation.rs:155-159). The meaningful commitment figures are
   * `pool.committed` (the shared-seat pool roll-up) and `draw.committed`
   * (this resource's own holding engagements), which the ADR states are
   * equal by the `resource_id = public_resource_id(preset_id)` invariant
   * (project.rs:67-69) — pinned by the shared-seat console test. */
  if (!object(v, ['scope', 'pool', 'seat', 'remainingTokens', 'draw']) || v.scope !== 'resource' || !amount(v.remainingTokens)
    || !object(v.pool, ['ceiling', 'period', 'committed', 'remaining']) || !amount(v.pool.ceiling) || !optionalText(v.pool.period, 64 * 1024) || !number(v.pool.committed) || !amount(v.pool.remaining)
    || !object(v.seat, ['quota', 'period', 'committed', 'remaining', 'status']) || !amount(v.seat.quota) || !optionalText(v.seat.period, 64 * 1024) || !number(v.seat.committed) || !amount(v.seat.remaining)
    || !['undeclared', 'declared', 'period_mismatch'].includes(v.seat.status) || !draw(v.draw)) throw new Error('invalid_native_response');
  return v;
}
export async function fetchResources(selected, after = '') {
  if ((selected !== null && !id(selected)) || (after && !id(after))) throw new Error('invalid_selection');
  const list = validateResources(await request(`/api/resources?limit=16${after ? `&after=${after}` : ''}`));
  const chosen = selected ?? list.resources[0]?.id ?? null;
  const budget = chosen === null ? null : validateBudget(await request(`/api/resources/${chosen}/budget`));
  for (const r of list.resources) remember(r.id, [r.framework, r.model, r.reasoning].filter(Boolean).join(' · '));
  return { ...list, selected: chosen, budget, resourceConsole: true };
}
export async function publishResource(resource, published) {
  const value = await request(`/api/resources/${resource.id}/publication`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ expectedRevision: resource.revision, published }) });
  if (!object(value, ['resourceId', 'published', 'revision']) || value.resourceId !== resource.id || value.published !== published || !revision(value.revision)) throw new Error('outcome_unknown');
  return value;
}

export function configurationView(location) { return /^\/console\/resources\/new\/?$/.test(location.pathname); }
export function configurationSelection(location) {
  const query = new URLSearchParams(location.search);
  const edit = query.has('resource_id');
  return { mode: edit ? 'edit' : 'create', id: selection(location, edit ? 'resource_id' : 'source_resource_id') };
}
export function validateConfiguration(value, selected) {
  if (!object(value, ['resource', 'choices', 'modelTier', 'modelRoles']) || value.resource?.id !== selected
    || !Array.isArray(value.choices) || value.choices.length > 256
    || value.choices.some((c) => !object(c, ['model', 'reasoning', 'tier', 'roles']) || !text(c.model, 256) || !optionalText(c.reasoning, 128) || !['lightweight', 'medium', 'strong'].includes(c.tier) || !Array.isArray(c.roles) || c.roles.length > 6 || !c.roles.every((r) => text(r, 64)))
    || !(value.modelTier === null || ['lightweight', 'medium', 'strong'].includes(value.modelTier)) || !Array.isArray(value.modelRoles) || value.modelRoles.length > 6 || !value.modelRoles.every((r) => text(r, 64))) throw new Error('invalid_native_response');
  if (!validResource(value.resource)) throw new Error('invalid_native_response');
  return value;
}
export async function fetchConfiguration(entry, after = '') {
  const value = await fetchResources(entry.id, after);
  const editor = value.selected === null ? null : validateConfiguration(await request(`/api/resources/${value.selected}/configuration`), value.selected);
  return { ...value, editor, configurationConsole: true, editing: entry.mode === 'edit' };
}
export async function configureResource(resource, create, changes) {
  const payload = { expectedRevision: resource.revision, ...changes };
  if (create) payload.sourceResourceId = resource.id;
  const value = await request(create ? '/api/resources' : `/api/resources/${resource.id}/configuration`, { method: create ? 'POST' : 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload) });
  if (!object(value, ['resourceId', 'revision', 'published']) || !/^resource_[a-f0-9]{24}$/.test(value.resourceId) || !revision(value.revision) || typeof value.published !== 'boolean' || (!create && value.resourceId !== resource.id) || (create && (!value.published || value.resourceId === resource.id))) throw new Error('outcome_unknown');
  return value;
}

/* The console account surface (MA-S3b). Exactly six keys per row — the
 * server's AccountRow: id, ordinal, state, revision, profile, readiness —
 * and no login or identity value; the validator's exact-key list is the
 * same one-way contract every other read carries. `readiness` is the
 * recorded observation's mode (`subscription`/`api_key`), else `unknown`;
 * no credential byte or probe output ever crosses. */
const ACCOUNT_STATES = ['preparing', 'active', 'uncertain', 'retired'];
const ACCOUNT_READINESS = ['subscription', 'api_key', 'unknown'];
const validAccount = (a) => !(!object(a, ['id', 'ordinal', 'state', 'revision', 'profile', 'readiness'])
  || !id(a.id) || !Number.isSafeInteger(a.ordinal) || a.ordinal < 1 || a.ordinal > 16
  || !ACCOUNT_STATES.includes(a.state) || !revision(a.revision)
  || a.profile !== 'codex-default-namespace-v1' || !ACCOUNT_READINESS.includes(a.readiness));
export function validateAccounts(v) {
  if (!object(v, ['at_ms', 'accounts', 'next_after']) || !number(v.at_ms) || !Array.isArray(v.accounts)
    || v.accounts.length > 16 || !(v.next_after === null || id(v.next_after))
    || v.accounts.some((a) => !validAccount(a))) throw new Error('invalid_native_response');
  return v;
}
export function accountsView(location) { return /^\/console\/accounts\/?$/.test(location.pathname); }
export async function fetchAccounts() {
  return validateAccounts(await request('/api/accounts'));
}
/* Every mutation reply is the SAME one-row envelope as the single read, so
 * the same validator governs it — the page never re-derives a row. */
export async function fetchAccount(id) {
  const v = await request(`/api/accounts/${encodeURIComponent(id)}`);
  if (!object(v, ['account']) || !validAccount(v.account)) throw new Error('invalid_native_response');
  return v.account;
}
export async function prepareAccount() {
  const v = await request('/api/accounts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ profile: 'codex-default-namespace-v1' }) });
  if (!object(v, ['account']) || !validAccount(v.account)) throw new Error('invalid_native_response');
  return v.account;
}
export async function retireAccount(id) {
  const v = await request(`/api/accounts/${encodeURIComponent(id)}/retire`, { method: 'POST' });
  if (!object(v, ['account']) || !validAccount(v.account)) throw new Error('invalid_native_response');
  return v.account;
}
export async function enrollAccountResource(id, model, reasoning, expectedRevision) {
  const payload = { model, expectedRevision };
  if (reasoning) payload.reasoning = reasoning;
  const v = await request(`/api/accounts/${encodeURIComponent(id)}/enrollment`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload) });
  if (!object(v, ['account']) || !validAccount(v.account)) throw new Error('invalid_native_response');
  return v.account;
}

/* Operator tasks (board #23, TS parity: backend-v2.js:13194-13332,
 * lib/task-store.js). The five statuses and four priorities are the retained
 * store's own sets, and they are NOT re-derived here: the server serves the
 * legal next-status list on every row (`allowed_transitions`), so the page
 * renders transitions only from what the server allowed, the same contract
 * the alerts page uses for its `next` array. A timestamp is a non-empty
 * string (the retained ISO-8601 wire spelling) or null. */
const TASK_PRIORITIES = ['p0', 'p1', 'p2', 'p3'];
const TASK_GRANULARITIES = ['epic', 'task', 'subtask'];
const stamp = (v) => v === null || text(v, 64);
const taskComment = (c) => object(c, ['author', 'text', 'ts'])
  && text(c.author, 128) && text(c.text, 4096) && text(c.ts, 64);
/* `next` is the SERVER's legal-transition list (hagency_store::operator_transitions),
 * so the page renders a move control only where the store allows one. It is
 * part of the exact-key set: a server that stopped serving it fails the whole
 * read rather than silently dropping the controls. */
const validTask = (t) => object(t, ['id', 'title', 'description', 'status', 'priority', 'granularity',
  'assignee', 'created_by', 'created_at', 'updated_at', 'started_at', 'completed_at', 'heartbeat_at',
  'waiting_reason', 'waiting_until', 'parent_id', 'labels', 'comments', 'next'])
  && id(t.id) && text(t.title, 255) && text(t.description, 4096)
  && TASK_STATES.includes(t.status) && TASK_PRIORITIES.includes(t.priority)
  && TASK_GRANULARITIES.includes(t.granularity)
  && optionalText(t.assignee, 128) && optionalText(t.created_by, 128)
  && text(t.created_at, 64) && text(t.updated_at, 64)
  && stamp(t.started_at) && stamp(t.completed_at) && stamp(t.heartbeat_at)
  && optionalText(t.waiting_reason, 1024) && optionalText(t.waiting_until, 64)
  && optionalText(t.parent_id, 64)
  && Array.isArray(t.labels) && t.labels.length <= 20 && t.labels.every((l) => text(l, 64))
  && Array.isArray(t.comments) && t.comments.length <= 100 && t.comments.every(taskComment)
  && Array.isArray(t.next) && t.next.length <= 2 && t.next.every((s) => TASK_STATES.includes(s));
export function validateTasks(v) {
  if (!object(v, ['at_ms', 'permissions', 'unavailable', 'tasks']) || !number(v.at_ms)
    || !object(v.permissions, ['configureResource']) || typeof v.permissions.configureResource !== 'boolean'
    || !Array.isArray(v.unavailable) || v.unavailable.length > 32 || v.unavailable.some((n) => !text(n, 64))
    || !Array.isArray(v.tasks) || v.tasks.length > 500 || v.tasks.some((t) => !validTask(t))) throw new Error('invalid_native_response');
  return v;
}
export function tasksView(location) { return /^\/console\/tasks\/?$/.test(location.pathname); }
export function projectBoardView(location) { return /^\/console\/project-board\/?$/.test(location.pathname); }
export async function fetchTasks() { return validateTasks(await request('/api/tasks')); }
export async function fetchAgentTasks(name) {
  if (!text(name, 128) || !name.trim()) throw new Error('invalid_selection');
  return validateTasks(await request(`/api/agents/${encodeURIComponent(name)}/tasks`));
}
const oneTask = (v) => { if (!object(v, ['ok', 'task']) || v.ok !== true || !validTask(v.task)) throw new Error('invalid_native_response'); return v.task; };
export async function createTask(body) {
  return oneTask(await request('/api/tasks', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }));
}
// The parameter is `task`, NOT `id`: a parameter named `id` shadows the
// module-level `id` validator above, so the guard read `if (!id(id))` on a
// STRING and threw `TypeError: id is not a function` BEFORE any request went
// out. Every task mutation through the console therefore died silently in the
// click handler — `act` caught it and flashed a toast, and the page simply
// never changed. (The live action walk found it: the comment control stayed
// enabled, the id was valid, and no /comments request was ever sent.)
export async function updateTask(task, patch) {
  if (!id(task)) throw new Error('invalid_selection');
  return oneTask(await request(`/api/tasks/${encodeURIComponent(task)}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(patch) }));
}
export async function deleteTask(task) {
  if (!id(task)) throw new Error('invalid_selection');
  return oneTask(await request(`/api/tasks/${encodeURIComponent(task)}`, { method: 'DELETE' }));
}
export async function transitionTask(task, status, extra = {}) {
  if (!id(task) || !TASK_STATES.includes(status)) throw new Error('invalid_selection');
  return oneTask(await request(`/api/tasks/${encodeURIComponent(task)}/transition`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...extra, status }) }));
}
export async function commentTask(task, comment) {
  if (!id(task)) throw new Error('invalid_selection');
  return oneTask(await request(`/api/tasks/${encodeURIComponent(task)}/comments`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(comment) }));
}
/* The project board: the retained envelope (`lib/project-board.js:buildProjectBoardSnapshot`)
 * carries `generatedAt`, `staleAfterMs`, `activityLimit`, `totals` and
 * `projects`; native additionally serves `unavailable`, naming every retained
 * column it has no source for. The board is validated structurally and its
 * unavailability is passed through as text — the page renders what the server
 * names, never a client-side guess at what is missing. */
export function validateProjectBoard(v) {
  if (!object(v, ['generatedAt', 'staleAfterMs', 'activityLimit', 'unavailable', 'totals', 'projects'])
    || !text(v.generatedAt, 64) || !number(v.staleAfterMs) || !number(v.activityLimit)
    || !Array.isArray(v.unavailable) || v.unavailable.length > 32 || v.unavailable.some((n) => !text(n, 64))
    || !object(v.totals, ['projects', 'agents', 'tasks'])
    || !number(v.totals.projects) || !number(v.totals.agents)
    || !Array.isArray(v.projects) || v.projects.length > 512
    || v.projects.some((p) => !object(p, ['id', 'name', 'agents', 'taskLanes']) || !text(p.id, 128) || !text(p.name, 128)
      || !Array.isArray(p.agents) || p.agents.some((a) => !text(a, 128))
      || !object(p.taskLanes, TASK_STATES) || TASK_STATES.some((s) => !number(p.taskLanes[s])))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchProjectBoard() { return validateProjectBoard(await request('/api/project-board')); }
/* Board #48: the requester-facing reads. Each validator pins the EXACT key set
 * the server serves, so a stale server or client fails loudly with
 * `invalid_native_response` instead of rendering half a page.
 *
 * `whitelisted` is tri-state on purpose and validated as such: `null` is "no
 * room was identified" (the retained route publishes room trust to a requester
 * never at all), which is NOT `false` ("this room is not trusted"). Collapsing
 * the two would turn "we did not ask" into an accusation.
 *
 * The three offer caps are `null` — the retained store's own "unset" encoding
 * (`lib/engagement-store.js:451-454`), never `0`. The page must render them as
 * an unstated cap, not as "zero tokens". */
const TIERS = ['lightweight', 'medium', 'strong'];
const OFFER_SERVING_KEYS = ['agent', 'framework', 'model', 'reasoning', 'tier', 'provisioningRequired'];
const OFFER_ROLE_KEYS = ['role', 'crossFamilyOk', 'budgetCapPerEngagement', 'rateCap', 'count', 'runningNow', 'serving', 'resources'];
const OFFER_RESOURCE_KEYS = ['id', 'name', 'framework', 'model', 'reasoning', 'tier'];
const validOffering = (v) => v === null || (object(v, OFFER_SERVING_KEYS)
  && optionalText(v.agent, 128) && optionalText(v.framework, 64) && optionalText(v.model, 256)
  && optionalText(v.reasoning, 128) && (v.tier === null || TIERS.includes(v.tier))
  && typeof v.provisioningRequired === 'boolean');
export function validateOfferBook(v) {
  if (!object(v, ['roles', 'whitelisted', 'projectRoomId']) || !Array.isArray(v.roles) || v.roles.length > 64
    || !(v.whitelisted === null || typeof v.whitelisted === 'boolean')
    || !optionalText(v.projectRoomId, 256)
    || v.roles.some((r) => !object(r, OFFER_ROLE_KEYS)
      || !text(r.role, 64) || typeof r.crossFamilyOk !== 'boolean'
      || !(r.budgetCapPerEngagement === null || number(r.budgetCapPerEngagement))
      || !(r.rateCap === null || number(r.rateCap))
      || !(r.count === null || number(r.count)) || !number(r.runningNow)
      || !validOffering(r.serving)
      || !Array.isArray(r.resources) || r.resources.length > 64
      || r.resources.some((x) => !object(x, OFFER_RESOURCE_KEYS)
        || !text(x.id, 128) || !text(x.name, 256) || !text(x.framework, 64) || !text(x.model, 256)
        || !optionalText(x.reasoning, 128) || !(x.tier === null || TIERS.includes(x.tier))))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchOfferBook(roomId = null) {
  return validateOfferBook(await request(`/api/offer-book${roomId ? `?projectRoomId=${encodeURIComponent(roomId)}` : ''}`));
}
const CONTRIBUTION_KEYS = ['agent', 'project', 'projectRoomId', 'ownerMxid', 'active', 'agentJoined', 'membershipCheckedAt'];
export function validateContributions(v) {
  if (!object(v, ['contributions']) || !Array.isArray(v.contributions) || v.contributions.length > 200
    || v.contributions.some((c) => !object(c, CONTRIBUTION_KEYS)
      || !text(c.agent, 128) || !text(c.project, 128) || !text(c.projectRoomId, 256) || !text(c.ownerMxid, 256)
      || typeof c.active !== 'boolean'
      || !(c.agentJoined === null || typeof c.agentJoined === 'boolean')
      || !(c.membershipCheckedAt === null || number(c.membershipCheckedAt)))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchContributions() {
  return validateContributions(await request('/api/contributions'));
}
const ROUTES = ['notWhitelisted', 'crossFamilyUnavailable', 'overOffer', 'overCeiling', 'autoJoin'];
export function validatePreview(v) {
  if (!object(v, ['route', 'autoJoin', 'agent', 'agentRemainingTokens'])
    || !ROUTES.includes(v.route) || typeof v.autoJoin !== 'boolean'
    || !optionalText(v.agent, 128)
    || !(v.agentRemainingTokens === null || number(v.agentRemainingTokens))) throw new Error('invalid_native_response');
  return v;
}
export async function fetchPreview(role, requestedTokens = null) {
  const query = `role=${encodeURIComponent(role)}${requestedTokens === null ? '' : `&requestedTokens=${encodeURIComponent(requestedTokens)}`}`;
  return validatePreview(await request(`/api/engagements/preview?${query}`));
}

/* Operator task graphs (board #47): the native console's task-graph routes
 * mirror the retained `/api/task-graphs` (backend-v2.js:15974-16020). A
 * graph is the TS `normalizeGraph` shape — id/owner/label/status plus a
 * `nodes` map keyed by node id with camelCase timestamps — served verbatim.
 * Validators refuse an unexpected top-level shape rather than render it. */
const GRAPH_STATUSES = ['active', 'complete', 'failed', 'cancelled'];
const NODE_STATUSES = ['pending', 'dispatched', 'active', 'complete', 'failed', 'skipped', 'cancelled'];
export function validateGraph(v) {
  if (!object(v, ['id', 'owner', 'label', 'status', 'nodes', 'createdAt', 'updatedAt', 'completedAt'])
    || !text(v.id, 255) || !text(v.owner, 255) || !text(v.label, 4000)
    || !GRAPH_STATUSES.includes(v.status)
    || !text(v.createdAt, 128) || !text(v.updatedAt, 128) || !optionalText(v.completedAt, 128)
    || typeof v.nodes !== 'object' || v.nodes === null || Array.isArray(v.nodes)) throw new Error('invalid_native_response');
  for (const n of Object.values(v.nodes)) {
    if (!object(n, ['id', 'assignee', 'description', 'depends_on', 'status', 'result', 'error', 'condition', 'message_id', 'dispatchedAt', 'completedAt', 'startedAt'])
      || !text(n.id, 255) || !text(n.assignee, 255) || !text(n.description, 4000)
      || !Array.isArray(n.depends_on) || n.depends_on.some((d) => !text(d, 255))
      || !NODE_STATUSES.includes(n.status)
      || !optionalText(n.error, 4000) || !optionalText(n.message_id, 255)
      || !optionalText(n.dispatchedAt, 128) || !optionalText(n.completedAt, 128) || !optionalText(n.startedAt, 128)) throw new Error('invalid_native_response');
  }
  return v;
}
export function validateGraphs(v) {
  if (!Array.isArray(v) || v.length > 1024) throw new Error('invalid_native_response');
  return v.map(validateGraph);
}
export async function fetchTaskGraphs(status = '') {
  return validateGraphs(await request(`/api/task-graphs${status ? `?status=${encodeURIComponent(status)}` : ''}`));
}
export async function fetchTaskGraph(id) {
  return validateGraph(await request(`/api/task-graphs/${encodeURIComponent(id)}`));
}
export async function createTaskGraph(body) {
  const v = await request('/api/task-graphs', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  if (v?.ok !== true) throw new Error('invalid_native_response');
  return validateGraph(v.graph);
}
export async function deleteTaskGraph(id) {
  const v = await request(`/api/task-graphs/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (v?.ok !== true) throw new Error('invalid_native_response');
  return validateGraph(v.graph);
}
export async function updateTaskGraphNode(id, nodeId, patch) {
  const v = await request(`/api/task-graphs/${encodeURIComponent(id)}/nodes/${encodeURIComponent(nodeId)}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(patch) });
  if (v?.ok !== true) throw new Error('invalid_native_response');
  return validateGraph(v.graph);
}
export function taskGraphsView(location) { return /^\/console\/task-graphs\/?$/.test(location.pathname); }
/* The Palpo owner download, imported through the console (TS: the "import
 * Palpo authorized configuration" step of /projects/new). Its own fetch: a
 * refusal carries the refused field, and saving may wait for a previous
 * transport to close. The answer is public facts only, never a token. */
export async function importPalpo(configuration, homeserver) {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 20000);
  try {
    const response = await fetch(`${ROOT}/api/palpo/import`, {
      method: 'POST', credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: abort.signal,
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ configuration, homeserver }),
    });
    let value = null;
    try { value = await response.json(); } catch { /* not JSON: handled below */ }
    if (response.status === 401) throw new Error('console_access_required');
    if (!response.ok || value?.ok !== true) {
      const refused = new Error(typeof value?.code === 'string' ? value.code : 'native_unavailable');
      if (typeof value?.field === 'string') refused.field = value.field.slice(0, 120);
      throw refused;
    }
    if (typeof value.fleetId !== 'string' || typeof value.serverName !== 'string') throw new Error('invalid_native_response');
    return value;
  } catch (error) {
    if (error.name === 'AbortError') throw new Error('outcome_unknown');
    throw error;
  } finally { clearTimeout(timer); }
}

/* ADR-189: the setup page. Status is read-only; check re-detects and, for a
 * signed-in coding agent, configures the runtime. */
async function setupCall(path, method) {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 30000);
  try {
    const response = await fetch(`${ROOT}/api/${path}`, {
      method, credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: abort.signal,
      headers: method === 'POST' ? { 'Content-Type': 'application/json' } : undefined,
      body: method === 'POST' ? '{}' : undefined,
    });
    let value = null;
    try { value = await response.json(); } catch { /* not JSON: handled below */ }
    if (response.status === 401) throw new Error('console_access_required');
    if (!response.ok || value?.ok !== true) throw new Error(typeof value?.code === 'string' ? value.code : 'native_unavailable');
    if (!Array.isArray(value.agents)) throw new Error('invalid_native_response');
    return value;
  } catch (error) {
    if (error.name === 'AbortError') throw new Error('outcome_unknown');
    throw error;
  } finally {
    clearTimeout(timer);
  }
}
export function fetchSetup() { return setupCall('setup', 'GET'); }
export function checkSetup() { return setupCall('setup/check', 'POST'); }
export async function offerResource(model, reasoning, tokens) {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 20000);
  try {
    const response = await fetch(`${ROOT}/api/setup/resource`, {
      method: 'POST', credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: abort.signal,
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ model, reasoning, tokens }),
    });
    let value = null;
    try { value = await response.json(); } catch { /* not JSON: handled below */ }
    if (response.status === 401) throw new Error('console_access_required');
    if (!response.ok || value?.ok !== true) throw new Error(typeof value?.code === 'string' ? value.code : 'native_unavailable');
    return value;
  } catch (error) {
    if (error.name === 'AbortError') throw new Error('outcome_unknown');
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

// ADR-191 owner ledger and delivered approvals. Every call uses the native
// session adapter above; runtime credentials never enter these read models.
function engagementPage(value, key, fields) {
  if (!object(value, [key, 'nextCursor']) || !Array.isArray(value[key]) || value[key].length > 50
      || !(value.nextCursor === null || id(value.nextCursor))
      || value[key].some(row => !object(row, fields) || !id(row.id))) throw new Error('invalid_native_response');
  return value;
}
export async function fetchServerEngagements(after = '') {
  if (after && !id(after)) throw new Error('invalid_selection');
  return engagementPage(await request(`/api/server-engagements?limit=50${after ? `&after=${after}` : ''}`), 'engagements',
    ['id', 'serverName', 'ownerMxid', 'coordinatorMxid', 'state', 'registrationGeneration', 'delegationRevision', 'delegationExpiresAtMs', 'allowSelfApproval', 'exportMxids', 'delegationPublication']);
}
export async function changeEngagementDelegation(change) {
  if (!id(change.serverEngagementId) || !number(change.expectedRevision) || change.expectedRevision < 1) throw new Error('invalid_selection');
  const value = await request(`/api/server-engagements/${change.serverEngagementId}/delegation`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(change),
  });
  if (!object(value, ['ok', 'engagement', 'publication']) || value.ok !== true || !value.engagement || value.engagement.id !== change.serverEngagementId) throw new Error('invalid_native_response');
  return value;
}
export async function fetchEngagementResources(fleet, after = '') {
  if (!id(fleet) || after && !id(after)) throw new Error('invalid_selection');
  return engagementPage(await request(`/api/server-engagements/${fleet}/resources?limit=50${after ? `&after=${after}` : ''}`), 'resources',
    ['id', 'serverEngagementId', 'resourceId', 'revision', 'allocatedTokens', 'retainedTokens', 'remainingTokens', 'overdrawn', 'period', 'periodKey', 'eligibleManagers']);
}
export async function fetchCoordinatorDecisions(fleet, after = '') {
  if (!id(fleet) || after && !id(after)) throw new Error('invalid_selection');
  return engagementPage(await request(`/api/server-engagements/${fleet}/decisions?limit=50${after ? `&after=${after}` : ''}`), 'decisions',
    ['id', 'serverEngagementId', 'requestId', 'agentAllocationId', 'agentName', 'projectId', 'projectOwner', 'decidedBy', 'approvedAtMs', 'resourceAllocationId', 'resourceId', 'requestedTokens', 'approvedTokens', 'state', 'reason', 'receivedAtMs', 'updatedAtMs']);
}
export async function fetchAllocationResources(after = '') {
  if (after && !id(after)) throw new Error('invalid_selection');
  return validateResources(await request(`/api/resources?limit=16${after ? `&after=${after}` : ''}`));
}
export async function contributeEngagementResource(grant) {
  if (!id(grant.serverEngagementId) || !id(grant.id) || !id(grant.resourceId)
      || !number(grant.allocatedTokens) || !number(grant.revision) || grant.revision < 1) throw new Error('invalid_selection');
  const value = await request(`/api/server-engagements/${grant.serverEngagementId}/resources`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(grant),
  });
  if (!object(value, ['ok']) || value.ok !== true) throw new Error('invalid_native_response');
  return value;
}
