'use client';

import { createContext, useContext, useEffect, useRef, useState } from 'react';
import DataStatus from '@/components/DataStatus';
import { makeDerive } from '@/lib/derive';
import { fetchLive, CONTRACT_SLICES } from '@/lib/api';
import * as fixture from '@/lib/mock-data';
import { NATIVE_MODE, serverEngagementsView, exchangeAccess, fetchNative, fetchResources, resourceView, publishResource, configurationView, configurationSelection, fetchConfiguration, configureResource, logoutNative, selection, alertsView, fetchAlerts, agentsView, fetchAgents, projectSidesView, fetchProjectSides, transitionAlert, approvalsView, accountsView, engagementsView } from '@/lib/native-api';
import { useLiveStream } from '@/lib/native-stream';

/*
 * One data context for the console, with provenance attached.
 *
 * The fixture is the INITIAL value, not a fallback of last resort. Two reasons:
 * the static export has no proxy at all and must render something true about the
 * design, and a first paint that is blank-then-populated makes every assertion
 * racy. So pages always have data; what changes is where it came from.
 *
 * `useData()` returns the fixture's exact export names plus `provenance`, so a
 * page reads `presetOf`, `committed` and `capability` without knowing or caring
 * which source is behind them. The derivations come from one factory
 * (lib/derive.js) for the same reason.
 */

const FIXTURE_DATA = {
  roleCapacity: fixture.roleCapacity,
  agents: fixture.agents,
  presets: fixture.presets,
  offers: fixture.offers,
  whitelist: fixture.whitelist,
  engagements: fixture.engagements,
  projectSides: fixture.projectSides,
  alerts: fixture.alerts,
  usage: fixture.usage,
  frameworks: [],
  /*
   * No seat fixture. A seat is DERIVED from how agents were launched, so inventing
   * one would be inventing a launch topology — and the page's own point is that the
   * derivation is a fact rather than a choice. Fixture mode therefore shows the
   * seat section empty with its reason, which is true: with no backend there is no
   * host to derive a seat from.
   */
  seats: [],
  seatKeyed: null,
  /*
   * No contribution fixture either, and for the same kind of reason. A binding is
   * the record that a project can REACH an agent; inventing one would invent an
   * access grant, and the roster's access column exists precisely to reconcile that
   * record against the allocations. With no backend there is no binding store, so
   * fixture mode shows the column as unanswered — which is true.
   */
  contributions: [],
  /*
   * Never fixtured. A fabricated invitation would invite the contributor to accept a
   * project that does not exist, and an empty fixture would tell them nobody is waiting.
   */
  invites: [],
  detected: fixture.detected,
  detectedAt: null,
  detectCaveat: null,
  usageLive: [],
  metering: null,
  usageTotals: null,
};

/** Everything a page can read, assembled from a data object. */
function assemble(data, provenance, errors, refresh = async () => {}) {
  const derived = makeDerive(data);
  /*
   * When GET /api/capability answered, the SERVER's judgement replaces the local
   * one — same function name, same shape, one page code path. The server is where
   * role-capacity.json is authoritative and where a project-side client would read
   * it, so two answers to "can I fill Reviewer" is exactly the drift this layer
   * exists to prevent.
   */
  const capability = data.capabilityRows
    ? () => data.capabilityRows
    : derived.capability;
  return {
    ...data,
    ...derived,
    capability,
    provenance,
    errors,
    // Re-fetch after a write. A page that mutates and then re-renders from stale
    // local state shows the user their intention rather than the result, which is
    // exactly the failure a live console must not have.
    refresh,
    // True while the first fetch is in flight. Pages do not gate on it — they show
    // fixture data — but the banner uses it so "no backend" is not announced
    // before the attempt has finished.
    loading: provenance.__loading === true,
  };
}

const DataContext = createContext(
  assemble(FIXTURE_DATA, Object.fromEntries([
    ...['agents', 'presets', 'frameworks', 'alerts'].map((k) => [k, 'fixture']),
    ...CONTRACT_SLICES.map((k) => [k, 'contract']),
  ]), {}),
);

export function useData() {
  return useContext(DataContext);
}

// The build-time choice preserves the existing provider's callable contract.
export const DataProvider = NATIVE_MODE ? NativeDataProvider : LegacyDataProvider;

function NativeDataProvider({ children }) {
  const initial = { nativeConsole: true, resourceConsole: false, configurationConsole: false, editor: null, editing: false, phase: 'loading', refreshing: false, requestKey: null, engagements: [], resources: [], roles: [], permissions: { publishResource: false, configureResource: false, manageLifecycle: false }, selected: null, report: null, budget: null, next_after: null, agents: [], sides: [], unavailable: [], error: null };
  const [state, setState] = useState(initial);
  const [logoutStatus, setLogoutStatus] = useState(null);
  const [action, setAction] = useState(null);
  const mutation = useRef(false);
  const admissionEpoch = useRef(0);
  const generation = useRef(0);
  const inFlight = useRef(0);
  const admitted = useRef(false);
  /* The live stream is open only while the console is admitted. EventSource
   * reconnects on its own, so a stream left open after End access would send
   * the dead credential again (the regression lane counts such reads). */
  const [streamOpen, setStreamOpen] = useState(false);
  const admit = (value) => { admitted.current = value; setStreamOpen(value); };
  const logoutPending = useRef(Promise.resolve());
  const endingAccess = useRef(false);
  const cursor = useRef('');
  /*
   * A page whose read the provider does not perform (approvals, accounts) still
   * needs to know WHEN the session was admitted — it must not fetch before the
   * ticket exchange, or its own read races the exchange and reports a 401 as
   * "access required" while the session is fine. This promise resolves once
   * admission completes, so such a page waits instead of exchanging for itself.
   */
  /*
   * Created ONCE, in the body guarded by the null check: `useRef(new Promise(
   * ...))` would run the executor on every render, so after the first re-render
   * `settle` would point at a fresh orphan promise while the one the pages await
   * never settles — the page would wait forever. The pair is kept together so
   * the resolver and the promise it belongs to can never come apart.
   */
  const ready = useRef(null);
  if (ready.current === null) {
    let settle;
    const promise = new Promise((resolve) => { settle = resolve; });
    ready.current = { promise, settle };
  }
  // Pages of the triage list already walked. Kept outside state because a new
  // page is a new requestKey, and the reset that follows would drop them.
  const triageRows = useRef([]);
  const load = async (after = cursor.current) => {
    if (!admitted.current) return;
    const mine = ++generation.current;
    inFlight.current += 1;
    let requestKey = null;
    try {
      const entry = configurationView(window.location) ? configurationSelection(window.location) : null;
      const alerts = alertsView(window.location);
      const roster = !alerts && agentsView(window.location);
      const sideList = !alerts && !roster && projectSidesView(window.location);
      /*
       * Approvals and accounts read for themselves — each page owns both its
       * read and its mutations — so the provider must fetch NOTHING here. It
       * used to fall through to the engagements branch and pull a 16-engagement
       * page PLUS a usage report for a page that renders neither: a read the
       * operator paid for and never saw.
       */
      const standalone = !alerts && !roster && !sideList
        && (approvalsView(window.location) || accountsView(window.location) || serverEngagementsView(window.location));
      /*
       * The engagements triage page reads the SAME list as /usage but renders no
       * report, so it must not request one. That is the other half of the same
       * defect: a wasted read is a read the service performed for nobody.
       */
      const triage = !alerts && !roster && !sideList && !standalone && engagementsView(window.location);
      const resources = !alerts && !roster && !sideList && !standalone && (entry !== null || resourceView(window.location));
      const requested = entry ? entry.id : alerts || roster || sideList || standalone ? null : selection(window.location, resources ? 'resource_id' : 'engagement_id');
      requestKey = JSON.stringify([entry ? `configuration:${entry.mode}` : alerts ? 'alerts' : roster ? 'agents' : sideList ? 'sides' : standalone ? 'standalone' : triage ? 'triage' : resources ? 'resources' : 'usage', requested, after]);
      setState((s) => s.requestKey === requestKey && ['ready', 'stale'].includes(s.phase)
        ? { ...s, refreshing: true, error: null }
        : { ...initial });
      const value = standalone ? {}
        : entry ? await fetchConfiguration(entry, after)
          : alerts ? await fetchAlerts()
            : roster ? await fetchAgents()
              : sideList ? await fetchProjectSides()
                : resources ? await fetchResources(requested, after)
                  : await fetchNative(requested, after, !triage);
      if (mine !== generation.current || !admitted.current) return;
      cursor.current = after;
      /*
       * The triage page walks its OWN cursor, and its state cards must report
       * the split of everything read so far rather than only the page on
       * screen — "pending 0" from a single 16-row window is a number the
       * operator would read as the whole service, and the triage read carries
       * no total to use instead. Later pages therefore keep the rows already
       * loaded; `firstPage` (after === '') starts them over. Held in a ref
       * because a new page is a new requestKey, and the reset above would
       * otherwise discard the earlier pages.
       */
      let merged = value;
      if (triage) {
        triageRows.current = after ? [...triageRows.current, ...(value.engagements ?? [])] : (value.engagements ?? []);
        merged = { ...value, engagements: triageRows.current };
      }
      setState({ ...initial, ...merged, phase: 'ready', requestKey });
      return value;
    } catch (error) {
      if (mine !== generation.current) return;
      if (error.message === 'console_access_required') admit(false);
      setState((s) => ['native_unavailable', 'busy'].includes(error.message) && s.requestKey === requestKey && ['ready', 'stale'].includes(s.phase)
        ? { ...s, phase: 'stale', refreshing: false, error: error.message }
        : { ...initial, phase: error.message === 'console_access_required' ? 'access' : 'error', error: error.message });
    } finally { inFlight.current -= 1; }
  };
  useEffect(() => {
    let stopped = false;
    const enter = async () => {
      const mine = ++generation.current;
      admissionEpoch.current += 1;
      admit(false);
      setLogoutStatus(null); setAction(null);
      setState({ ...initial });
      try {
        await exchangeAccess(window.location, window.history, logoutPending.current);
        if (!stopped && mine === generation.current) { admit(true); ready.current.settle(); await load(''); }
      } catch (error) { if (!stopped && mine === generation.current) setState({ ...initial, phase: 'access', error: error.message }); }
      /*
       * Settle `ready` on BOTH outcomes. A page that reads for itself must not
       * wait forever when admission failed — it fetches, fails, and reports the
       * denial in its own words. Leaving the promise pending on the failure arm
       * would strand approvals/accounts in `loading` with no explanation.
       */
      finally { ready.current.settle(); }
    };
    void enter();
    /* After End access the credential is gone: focus, visibility and
     * popstate must not wave a dead token at the API (the refused reads
     * the regression lane names). `renew` is the re-admission path and
     * stays ungated by design. */
    const refresh = () => { if (!stopped && admitted.current && inFlight.current === 0 && document.visibilityState === 'visible') void load(); };
    const navigate = () => { if (!stopped && admitted.current) void load(); };
    const renew = () => { if (!stopped && window.location.hash) void enter(); };
    const timer = setInterval(refresh, 15000);
    window.addEventListener('focus', refresh);
    window.addEventListener('popstate', navigate);
    window.addEventListener('hashchange', renew);
    document.addEventListener('visibilitychange', refresh);
    return () => { stopped = true; admitted.current = false; generation.current += 1; clearInterval(timer); window.removeEventListener('focus', refresh); window.removeEventListener('popstate', navigate); window.removeEventListener('hashchange', renew); document.removeEventListener('visibilitychange', refresh); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // #26 live updates: the SSE stream says a CATEGORY changed; this provider
  // refetches its own bounded read — the retained dashboard's division. The
  // stream signal rides the same in-flight and visibility guards the 15 s
  // poll uses, so a live tab and an idle tab behave identically.
  const liveRefresh = useRef(() => {});
  liveRefresh.current = () => { if (admitted.current && inFlight.current === 0 && document.visibilityState === 'visible') void load(); };
  useLiveStream((category) => {
    if (!['agents', 'tasks', 'alerts'].includes(category)) return;
    liveRefresh.current();
  }, NATIVE_MODE && streamOpen);
  const choose = (value) => {
    if (configurationView(window.location)) {
      window.history.pushState(window.history.state, '', `/console/resources/new/?source_resource_id=${encodeURIComponent(value)}`);
      void load(''); return;
    }
    const resources = resourceView(window.location);
    window.history.pushState(window.history.state, '', `/console/${resources ? 'resources/?resource_id' : 'usage/?engagement_id'}=${encodeURIComponent(value)}`);
    void load();
  };
  const logout = async () => {
    if (endingAccess.current) return;
    endingAccess.current = true;
    admit(false);
    admissionEpoch.current += 1;
    const mine = ++generation.current;
    setLogoutStatus('pending');
    if (mutation.current) setAction((a) => a ? { ...a, kind: 'unknown' } : a);
    setState({ ...initial, phase: 'access' });
    const pending = logoutNative();
    logoutPending.current = pending;
    try { await pending; if (mine === generation.current) setLogoutStatus('ended'); }
    catch (error) { if (mine === generation.current) { setLogoutStatus(error.message === 'busy' ? 'busy' : 'unknown'); setState({ ...initial, phase: 'access', error: error.message }); } }
    finally { endingAccess.current = false; if (logoutPending.current === pending) logoutPending.current = Promise.resolve(); }
  };
  const publish = async (resource, published) => {
    if (!admitted.current || mutation.current) return;
    mutation.current = true;
    const epoch = admissionEpoch.current;
    const identity = { id: resource.id, label: `${resource.framework} · ${resource.model}` };
    setAction({ ...identity, kind: 'pending' });
    try {
      await publishResource(resource, published);
      if (epoch !== admissionEpoch.current) return;
      setAction({ ...identity, kind: 'saved' });
      await load();
    } catch (error) {
      if (epoch !== admissionEpoch.current) return;
      const kind = error.message === 'resource_revision_conflict' ? 'conflict' : error.message === 'busy' ? 'busy' : ['outcome_unknown', 'native_unavailable', 'invalid_native_response'].includes(error.message) ? 'unknown' : 'refused';
      setAction({ ...identity, kind, error: error.message });
      if (error.message === 'console_access_required') { admit(false); generation.current += 1; setState({ ...initial, phase: 'access', error: error.message }); }
    } finally { mutation.current = false; }
  };
  const configure = async (resource, changes) => {
    if (!admitted.current || mutation.current) return;
    mutation.current = true;
    const epoch = admissionEpoch.current;
    const identity = { id: resource.id, label: `${resource.framework} · ${resource.model}`, configuration: true };
    setAction({ ...identity, kind: 'pending' });
    try {
      const result = await configureResource(resource, !state.editing, changes);
      if (epoch !== admissionEpoch.current) return;
      setAction({ ...identity, kind: 'saved', result });
      await load();
    } catch (error) {
      if (epoch !== admissionEpoch.current) return;
      const kind = error.message === 'resource_revision_conflict' ? 'conflict' : error.message === 'busy' ? 'busy' : ['outcome_unknown', 'native_unavailable', 'invalid_native_response'].includes(error.message) ? 'unknown' : 'refused';
      setAction({ ...identity, kind, error: error.message });
      if (error.message === 'console_access_required') { admit(false); generation.current += 1; setState({ ...initial, phase: 'access', error: error.message }); }
    } finally { mutation.current = false; }
  };
  /* One display-state transition on an alert (ADR-124 amendment). The
   * buttons come ONLY from the served `next` array, so this mutator can
   * never fire a route the server does not offer. */
  const transition = async (key, to) => {
    if (!admitted.current || mutation.current) return;
    mutation.current = true;
    const epoch = admissionEpoch.current;
    const identity = { id: key, label: to };
    setAction({ ...identity, kind: 'pending' });
    try {
      await transitionAlert(key, to);
      if (epoch !== admissionEpoch.current) return;
      setAction({ ...identity, kind: 'saved' });
      await load();
    } catch (error) {
      if (epoch !== admissionEpoch.current) return;
      const kind = error.message === 'bad_transition' ? 'refused' : error.message === 'busy' ? 'busy' : ['outcome_unknown', 'native_unavailable', 'invalid_native_response'].includes(error.message) ? 'unknown' : 'refused';
      setAction({ ...identity, kind, error: error.message });
      if (error.message === 'console_access_required') { admit(false); generation.current += 1; setState({ ...initial, phase: 'access', error: error.message }); }
    } finally { mutation.current = false; }
  };
  return <DataContext.Provider value={{ ...state, action, publish, configure, transition, logoutStatus, ready: ready.current.promise, choose, refresh: () => load(), nextPage: () => load(state.next_after), firstPage: () => load(''), logout }}>{children}</DataContext.Provider>;
}

function LegacyDataProvider({ children }) {
  const [state, setState] = useState(() => assemble(
    FIXTURE_DATA,
    {
      __loading: true,
      ...Object.fromEntries(['agents', 'presets', 'frameworks', 'alerts'].map((k) => [k, 'fixture'])),
      ...Object.fromEntries(CONTRACT_SLICES.map((k) => [k, 'contract'])),
    },
    {},
  ));

  /*
   * A generation counter, because an older response must never overwrite a newer one.
   *
   * `refresh()` is called after every write, and the effect calls `load()` on mount.
   * With no ordering, two overlapping fetches resolve in whatever order the network
   * gives them: press Revoke, the refresh renders the ended engagement, then the
   * initial fetch completes and puts it back on screen as active. The user sees their
   * change undo itself.
   */
  const generation = useRef(0);
  const inFlight = useRef(0);

  const load = async () => {
    const mine = ++generation.current;
    /*
     * `?data=fixture` forces fixture mode without stopping the backend.
     *
     * Needed because the two suites want opposite things from one dev server:
     * check-switches asserts against fixture rows (contract slices are empty
     * against a live backend, so its cell-layout checks had nothing to inspect and
     * failed for the right reason), while live-ux asserts against real payloads.
     * Also useful by hand: it makes the provenance claim falsifiable in both
     * directions rather than only when a backend happens to be down.
     */
    const stale = () => mine !== generation.current;
    if (typeof window !== 'undefined'
      && new URLSearchParams(window.location.search).get('data') === 'fixture') {
      setState(assemble(
        FIXTURE_DATA,
        {
          ...Object.fromEntries(['agents', 'presets', 'frameworks', 'alerts'].map((k) => [k, 'fixture'])),
          ...Object.fromEntries(CONTRACT_SLICES.map((k) => [k, 'contract'])),
        },
        {},
        load,
      ));
      return;
    }
    inFlight.current += 1;
    try {
      const { data, provenance, errors } = await fetchLive();
      if (stale()) return;
      setState(assemble({ ...FIXTURE_DATA, ...data }, provenance, errors, load));
    } catch (e) {
      // fetchLive already degrades per slice, so reaching here means something
      // structural. Keep the fixture and say why rather than rendering nothing.
      if (stale()) return;
      setState((prev) => assemble(FIXTURE_DATA, { ...prev.provenance, __loading: false }, { all: e.message }, load));
    } finally {
      inFlight.current -= 1;
    }
  };

  useEffect(() => {
    let cancelled = false;
    void load();
    // Automatic observations never overlap a load. Explicit refresh() still
    // starts a newer generation, so a completed write can supersede old reads.
    const refreshVisible = () => {
      if (!cancelled && document.visibilityState === 'visible' && inFlight.current === 0) void load();
    };
    const timer = setInterval(refreshVisible, 15_000);
    window.addEventListener('focus', refreshVisible);
    document.addEventListener('visibilitychange', refreshVisible);
    return () => {
      cancelled = true;
      clearInterval(timer);
      window.removeEventListener('focus', refreshVisible);
      document.removeEventListener('visibilitychange', refreshVisible);
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return <DataContext.Provider value={state}>{children}</DataContext.Provider>;
}

/**
 * The banner that names what is real on this page.
 *
 * Not decoration. Four slices have endpoints and five do not, so a console that
 * looked uniformly live would misrepresent five of its own numbers. `slices` is
 * what this page actually reads, so the banner is specific rather than global.
 */
export function Provenance({ slices }) {
  const { provenance, errors, loading } = useData();
  return <DataStatus slices={slices} provenance={provenance} errors={errors} loading={loading} />;
}
