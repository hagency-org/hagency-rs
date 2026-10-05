'use client';

/*
 * Native engagements: a READ-ONLY triage list over the same
 * /console/api/engagements read the usage page selects from — no new data
 * path, the page just renders the list slice `Data.jsx` already carries
 * (`fetchNative` returns `engagements` + `next_after`; pagination rides
 * `nextPage`/`firstPage`). No create, verdict, revoke or whitelist action:
 * those mutate enforcement and need their own reviewed decision; buttons
 * that would 404 lie (the same rule as the alerts page).
 *
 * The retained page's route reasons (notWhitelisted / overOffer /
 * overCeiling) have no native counterpart — the whitelist is the retained
 * fleet model's admission surface. The native analogue of "awaiting my
 * decision" is the engagement STATE column (pending → reserved/active …),
 * which is exactly what renders here.
 */
import { useMemo, useState } from 'react';
import PageHead from '@/components/PageHead';
import NativeServerEngagements from '@/components/NativeServerEngagements';
import NativeSettlement from '@/components/NativeSettlement';
import NativeStatusStrip from '@/components/NativeStatusStrip';
import { NativeAccessNotice } from '@/components/NativeUsage';
import { useT } from '@/components/Prefs';
import { errorText } from '@/lib/i18n';
import { useData } from '@/components/Data';
import { fmtTokens } from '@/lib/mock-data';
import { raiseEngagementAllocation, retireEngagement, retryEngagementCleanup } from '@/lib/native-api';

const NATIVE_STATES = ['pending', 'reserved', 'active', 'rejected', 'revoked', 'failed'];
/* The command id is the store's idempotency key: minted here, never by the
 * route. One per operator act, so a double-submit replays rather than acts. */
const newCommand = () => `console_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;

/* `lead` renders right under the page header: the decisions waiting on the
 * operator come before the triage list they belong to. */
export default function NativeEngagements({ lead = null, pending = null, other = null, listHidden = false }) {
  const t = useT();
  const data = useData();
  const { phase, error, refreshing, engagements = [], next_after: nextAfter } = data;
  const [state, setState] = useState('all');
  const [agent, setAgent] = useState('all');
  // The exile rule (AgentActions.jsx): a destructive control asks first and
  // says what happened. `confirming` holds `{id, kind}` — never a bare flag,
  // so a confirmation can never be carried onto a different row.
  const [confirming, setConfirming] = useState(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState(null);
  /* ADR-186 §C: the open "Add tokens" form — `{id, amount}`, one row at a
   * time, never carried onto another row. */
  const [topUp, setTopUp] = useState(null);

  const agentNames = useMemo(
    () => [...new Set(engagements.map((e) => e.agentName))].filter(Boolean),
    [engagements],
  );
  const counts = useMemo(() => {
    const out = Object.fromEntries(NATIVE_STATES.map((s) => [s, 0]));
    for (const e of engagements) out[e.state] = (out[e.state] ?? 0) + 1;
    return out;
  }, [engagements]);
  const rows = useMemo(
    () => engagements
      .filter((e) => state === 'all' || e.state === state)
      .filter((e) => agent === 'all' || e.agentName === agent),
    [engagements, state, agent],
  );

  /* Retire is the store's `end(..., revoke = true)` guard (domain.rs:1525-1530):
   * pending, reserved or active. Anything terminal is refused with
   * `engagement_not_live`, so no button is offered for it. */
  const retirable = (e) => ['pending', 'reserved', 'active'].includes(e.state);
  /* The cleanup retry's precondition (domain.rs:1505-1510): the engagement is
   * revoked AND its `retire` effect is failed. The list read exposes that as
   * `cleanup` — TS's own `['failed','pending'].includes(withdrawal.state)`
   * arm, with native's four-value column in place of the withdrawal record.
   * A retirement that failed waits for this operator act: there is no sweeper
   * and no timer (engagements.rs:12). */
  const retryable = (e) => e.state === 'revoked' && ['pending', 'uncertain'].includes(e.cleanup);
  /* ADR-186 §C: only an engagement that holds an allocation can be topped
   * up — the store's own reserved/active guard; a paused one is active. */
  const raisable = (e) => e.coordinatorManaged !== true && ['reserved', 'active'].includes(e.state);

  async function addTokens(id) {
    if (busy || topUp?.id !== id) return;
    const amount = topUp.amount.trim();
    if (!/^[1-9][0-9]{0,15}$/.test(amount)) { setNote(t('nv.amountInvalid')); return; }
    setBusy(true);
    setNote(null);
    try {
      await raiseEngagementAllocation(id, newCommand(), Number(amount));
      setTopUp(null);
      setNote(t('quota.addedMsg', { n: fmtTokens(Number(amount)) }));
      await data.refresh();
    } catch (error) {
      setNote(error.message === 'agent_lifecycle_scope_required'
        ? t('ng.scopeRequired')
        : `${t('ng.actionFailed')} (${errorText(t, error.message)})${error.detail ? `: ${error.detail}` : ''}`);
    } finally {
      setBusy(false);
    }
  }

  async function act(id, kind) {
    if (busy) return;
    setBusy(true);
    setNote(null);
    try {
      const receipt = kind === 'retire'
        ? await retireEngagement(id, newCommand())
        : await retryEngagementCleanup(id, newCommand());
      setConfirming(null);
      setNote(kind === 'retire'
        ? t('ng.retiredMsg', { state: receipt.state, cleanup: receipt.cleanup })
        : t('ng.retriedMsg', { state: receipt.state, cleanup: receipt.cleanup }));
      await data.refresh();
    } catch (error) {
      setNote(error.message === 'agent_lifecycle_scope_required'
        ? t('ng.scopeRequired')
        : `${t('ng.actionFailed')} (${errorText(t, error.message)})`);
    } finally {
      setBusy(false);
    }
  }

  if (phase === 'error') {
    return (
      <section className="panel" role="alert">
        <h2>{t('nu.failed')}</h2>
        <p>{t(error === 'not_found' ? 'nu.notFound' : 'nu.retryHelp')}</p>
        <button className="btn" onClick={data.refresh}>{t('nu.refresh')}</button>
      </section>
    );
  }
  /* Item 6: the access notice with the CLI command, not a blank screen. */
  if (phase === 'access') return <>
    <PageHead title={t('se.agentAllocations')} sub={t('ng.readonly')}><NativeStatusStrip /></PageHead>
    <NativeAccessNotice />
  </>;

  return (
    <div data-native-state={phase} aria-busy={refreshing === true}>
      {refreshing && <p role="status">{t('nu.refreshing')}</p>}

      {/* Item 1: one heading per page — the h1 carries the title, the
       * read-only note rides its `sub`, and the duplicate h2 is gone. */}
      <PageHead title={t('se.agentAllocations')} sub={t('ng.readonly')}><NativeStatusStrip /></PageHead>
      <NativeServerEngagements decisionsOnly />
      {lead}
      <div className="eng-list" hidden={listHidden}>

      {/* The counts are the split of the rows loaded so far — page one plus any
          further pages walked — and they SAY so, because a bare "pending 0" from
          a single window reads as a fact about the whole service, which it is
          not: there is no total in the triage read to report instead. */}
      <div className="cards">
        {NATIVE_STATES.map((s) => (
          <div className="card" key={s}>
            <div className="cap">{s}</div>
            <div className={`val${s === 'pending' && counts[s] > 0 ? ' warn' : ''}`}>{counts[s]}</div>
          </div>
        ))}
      </div>
      <p className="dim" style={{ fontSize: 12 }}>{t('ng.countScope')}</p>
      {pending}

      <div className="btn-row" style={{ margin: '22px 0 12px' }}>
        <label style={{ fontSize: 12, color: 'var(--ink-dim)' }}>
          {t('col.state')}{' '}
          <select value={state} onChange={(e) => setState(e.target.value)}>
            <option value="all">{t('common.all')}</option>
            {NATIVE_STATES.map((s) => <option key={s} value={s}>{s}</option>)}
          </select>
        </label>
        <label style={{ fontSize: 12, color: 'var(--ink-dim)' }}>
          {t('col.agent')}{' '}
          <select value={agent} onChange={(e) => setAgent(e.target.value)}>
            <option value="all">{t('common.all')}</option>
            {agentNames.map((a) => <option key={a} value={a}>{a}</option>)}
          </select>
        </label>
        <span className="spacer" style={{ flex: 1 }} />
        <span className="sub dim" style={{ fontSize: 12 }}>
          {t('common.shown', { a: rows.length, b: engagements.length })}
        </span>
      </div>

      {/* Item 7: the list starts empty — "no engagements" is a ready-state
       * fact, not a first paint. */}
      {phase === 'loading' ? (
        <p role="status">{t('ng.loading')}</p>
      ) : rows.length === 0 ? (
        <div className="empty">
          <div className="big">{t('ng.none')}</div>
        </div>
      ) : (
        <div className="list">
          <table>
            <thead>
              <tr>
                <th>{t('col.state')}</th>
                <th>{t('col.agent')}</th>
                <th>{t('col.project')}</th>
                <th>{t('col.role')}</th>
                <th className="num">{t('col.requested')}</th>
                <th className="num">{t('col.allocated')}</th>
                <th className="num">{t('quota.spent')}</th>
                <th className="num">{t('col.remaining')}</th>
                <th>{t('col.action')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((e) => (
                <tr key={e.id} data-engagement-row={e.id}>
                  <td>
                    {e.state}
                    {/* ADR-186 §B: the allocation is used up; queued work
                        waits for a top-up. */}
                    {e.quotaPaused && <> <span className="stranded warn-chip" data-quota-paused>{t('quota.paused')}</span></>}
                  </td>
                  <td>
                    {/* The agent name reaches the usage page for THIS engagement
                        — the drill-down the reader expects a triage row to have. */}
                    <a href={`/console/usage/?engagement_id=${encodeURIComponent(e.id)}`}>{e.agentName}</a>
                  </td>
                  <td>{e.projectName ?? '—'}</td>
                  <td>{e.role}</td>
                  <td className="num dim">{fmtTokens(e.requestedTokens)}</td>
                  <td className="num" data-allocated={e.allocatedTokens}>{fmtTokens(e.allocatedTokens)}</td>
                  {/* Known fresh spend; unknown is said, never a zero. */}
                  <td className="num dim" data-spent={e.spentTokens === null ? 'unknown' : e.spentTokens}>{e.spentTokens === null ? t('nu.unknown') : fmtTokens(e.spentTokens)}</td>
                  {/* Board #60 item 3: what is LEFT on the resource behind the
                      agent, so over-commitment shows BEFORE the decision. Null
                      is unknown (no ceiling declared), never a zero allowance;
                      when it is under the ask the figure is marked, and a
                      pending row with no owner binding says so. */}
                  <td className="num dim" data-remaining={e.agentRemainingTokens === null ? 'unknown' : e.agentRemainingTokens}>
                    {e.agentRemainingTokens === null ? t('nu.unknown') : fmtTokens(e.agentRemainingTokens)}
                    {e.agentRemainingTokens !== null && e.agentRemainingTokens < e.requestedTokens && <span className="overqual" title={t('ng.overCommitted', { n: fmtTokens(e.agentRemainingTokens) })}> ⚠</span>}
                    {e.ownerBindingRequired && <> <span className="stranded warn-chip">{t('ng.ownerRequired')}</span></>}
                  </td>
                  <td>
                    {/* #44 item 10 — the row links to its own usage detail. */}
                    <a href={`/console/usage/?engagement_id=${encodeURIComponent(e.id)}`}>{t('ng.viewUsage')}</a>{' '}
                    {e.coordinatorManaged && e.state === 'revoked' && <NativeSettlement agent={e} onSaved={data.refresh} />}
                    {/* Exile + confirm (AgentActions.jsx, lane apwait #45): the
                        confirmation names THIS row's id, so a slip on one row
                        can never retire another. Both fixes share the cell. */}
                    {confirming?.id === e.id ? (
                      <span className="btn-row tight">
                        <span className="dim">{confirming.kind === 'retire' ? t('ng.confirmRetire') : t('ng.confirmRetry')}</span>
                        <button type="button" className="btn-s danger" disabled={busy}
                          onClick={() => act(e.id, confirming.kind)}>{t('ng.confirm')}</button>
                        <button type="button" className="btn-s" disabled={busy}
                          onClick={() => setConfirming(null)}>{t('ng.cancel')}</button>
                      </span>
                    ) : topUp?.id === e.id ? (
                      <span className="btn-row tight" data-top-up={e.id}>
                        <input
                          data-top-up-amount
                          inputMode="numeric"
                          aria-label={t('quota.addAmount')}
                          value={topUp.amount}
                          disabled={busy}
                          onChange={(event) => { setNote(null); setTopUp({ id: e.id, amount: event.target.value }); }}
                          style={{ width: '9em' }}
                        />
                        {/* The row's own remaining figure is the headroom the
                            store checks a top-up against. */}
                        <button type="button" className="btn-s" disabled={busy || e.agentRemainingTokens === null || e.agentRemainingTokens < 1}
                          onClick={() => { setNote(null); setTopUp({ id: e.id, amount: String(e.agentRemainingTokens) }); }}>
                          {t('nv.allRemaining')}
                        </button>
                        <button type="button" className="btn-s primary" disabled={busy}
                          onClick={() => addTokens(e.id)}>{t('quota.add')}</button>
                        <button type="button" className="btn-s" disabled={busy}
                          onClick={() => setTopUp(null)}>{t('ng.cancel')}</button>
                      </span>
                    ) : (
                      <>
                        {raisable(e) && (
                          <button type="button" className="btn-s" disabled={busy}
                            onClick={() => { setNote(null); setConfirming(null); setTopUp({ id: e.id, amount: '' }); }}>
                            {t('quota.addTokens')}
                          </button>
                        )}{' '}
                        {retirable(e) && (
                          <button type="button" className="btn-s danger" disabled={busy}
                            onClick={() => { setNote(null); setConfirming({ id: e.id, kind: 'retire' }); }}>
                            {t('ng.retire')}
                          </button>
                        )}
                        {retryable(e) && (
                          <button type="button" className="btn-s" disabled={busy}
                            onClick={() => { setNote(null); setConfirming({ id: e.id, kind: 'cleanup-retry' }); }}>
                            {t('ng.retryCleanup')}
                          </button>
                        )}
                      </>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* The outcome of the last row action (retire, cleanup retry, add
          tokens), including a refusal's own explanation. */}
      {note && <p role="status" data-engagement-note>{note}</p>}
      <div className="btn-row" style={{ marginTop: 14 }}>
        <button className="btn" onClick={data.refresh}>{t('nu.refresh')}</button>
        <button className="btn" onClick={data.firstPage}>{t('nu.firstPage')}</button>
        <button className="btn" disabled={nextAfter === null} onClick={data.nextPage}>{t('ng.nextPage')}</button>
      </div>
      </div>
      {other}
    </div>
  );
}
