'use client';

/*
 * The native verdict surface (board #16, parity backend-v2.js:15161-15221):
 * the operator approves or refuses a pending engagement from this console.
 * Approve reaches the same store verdict the project-side Matrix approval
 * reaches — the server answers the bounded receipt and provisioning is
 * enqueued; refuse rides the existing /api/agents/{id}/refuse route. The
 * candidate read names the stored resource (the retained project-definition
 * choice); the command id is minted client-side and is the store's
 * idempotency key, never the route's.
 *
 * ADR-186 §A: the approval chooses its amount. The field starts at the
 * request; "All remaining" fills in the candidate's `remainingTokens`, the
 * same smallest ceiling/seat/pool headroom the store checks the approval
 * against. An unchanged amount is sent as the plain approval (no
 * `allocatedTokens`), and a refusal shows the store's own explanation.
 */
import { useEffect, useState } from 'react';
import { nativeRequest } from '@/lib/native-api';
import { useT } from '@/components/Prefs';
import { errorText } from '@/lib/i18n';
import { useData } from '@/components/Data';
import { fmtTokens } from '@/lib/mock-data';

const newCommand = () => `console_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;

function PendingRow({ e, onDone }) {
  const t = useT();
  const [candidates, setCandidates] = useState(null);
  const [note, setNote] = useState(null);
  const [busy, setBusy] = useState(false);
  // Refuse is destructive and irreversible; approve is not. Only the
  // destructive arm asks first (AgentActions.jsx's rule).
  const [confirming, setConfirming] = useState(false);
  const [amount, setAmount] = useState(String(e.requestedTokens ?? ''));
  useEffect(() => {
    let live = true;
    nativeRequest(`/api/engagements/${encodeURIComponent(e.id)}/candidates`)
      .then((v) => { if (live) setCandidates(v); })
      .catch((error) => { if (live) setNote(error.message); });
    return () => { live = false; };
  }, [e.id]);
  const granted = /^[1-9][0-9]{0,15}$/.test(amount.trim()) ? Number(amount.trim()) : null;
  const decide = async (kind) => {
    if (busy) return;
    if (kind === 'approve' && granted === null) {
      setNote(t('nv.amountInvalid'));
      return;
    }
    setBusy(true);
    try {
      const path = kind === 'approve'
        ? `/api/engagements/${encodeURIComponent(e.id)}/approve`
        : `/api/agents/${encodeURIComponent(e.id)}/refuse`;
      const body = { commandId: newCommand() };
      if (kind === 'approve' && granted !== e.requestedTokens) body.allocatedTokens = granted;
      await nativeRequest(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      setNote(null);
      onDone(kind === 'approve' ? t('nv.approved') : t('nv.refusedMsg'));
    } catch (error) {
      setNote(error.message === 'agent_lifecycle_scope_required'
        ? t('nv.scopeRequired')
        : `${t('nv.decideFailed')} (${errorText(t, error.message)})${error.detail ? `: ${error.detail}` : ''}`);
    } finally {
      setBusy(false);
    }
  };
  const candidate = candidates?.candidates?.[0] ?? null;
  return (
    <tr>
      <td>{e.agentName}</td>
      <td className="dim">{e.role}</td>
      <td className="dim">
        {candidate
          ? `${candidate.resource} · ${candidate.framework} · ${candidate.model}`
            + (candidate.remainingTokens === null || candidate.remainingTokens === undefined
              ? '' : ` · ${t('nv.remaining')} ${fmtTokens(candidate.remainingTokens)}`)
          : (note ?? '…')}
      </td>
      <td>
        {candidates?.locked
          ? <span className="dim">{t('nv.locked')}</span>
          : confirming ? (
            <span className="btn-row tight">
              <span className="dim">{t('nv.confirmRefuse')}</span>
              <button className="btn-s danger" type="button" disabled={busy} onClick={() => decide('refuse')}>{t('nv.confirm')}</button>
              <button className="btn-s" type="button" disabled={busy} onClick={() => setConfirming(false)}>{t('nv.cancel')}</button>
            </span>
          ) : (
            <div className="btn-row">
              <label className="dim" style={{ display: 'inline-flex', gap: 6, alignItems: 'center' }}>
                {t('nv.amount')}
                <input
                  data-approve-amount
                  inputMode="numeric"
                  aria-label={t('nv.amount')}
                  value={amount}
                  disabled={busy}
                  onChange={(event) => { setNote(null); setAmount(event.target.value); }}
                  style={{ width: '9em' }}
                />
              </label>
              <button
                className="btn-s"
                type="button"
                disabled={busy || !candidate || candidate.remainingTokens === null || candidate.remainingTokens === undefined || candidate.remainingTokens < 1}
                onClick={() => { setNote(null); setAmount(String(candidate.remainingTokens)); }}
              >{t('nv.allRemaining')}</button>
              <button className="btn-s primary" type="button" disabled={busy || !candidate} onClick={() => decide('approve')}>{t('en.approve')}</button>
              <button className="btn-s danger" type="button" disabled={busy} onClick={() => { setNote(null); setConfirming(true); }}>{t('en.reject')}</button>
            </div>
          )}
        {candidates && note && <p role="alert" className="warn-text">{note}</p>}
      </td>
    </tr>
  );
}

export default function NativeVerdict() {
  const t = useT();
  const data = useData();
  const [flash, setFlash] = useState(null);
  const [audit, setAudit] = useState(null);
  useEffect(() => {
    let live = true;
    nativeRequest('/api/engagements/audit')
      .then((v) => { if (live) setAudit(v.audit ?? []); })
      .catch(() => { if (live) setAudit([]); });
    return () => { live = false; };
  }, [flash]);
  if (data.phase === 'error' || data.phase === 'access') return null;
  const pending = (data.engagements ?? []).filter((e) => e.state === 'pending' && e.coordinatorManaged !== true);
  return (
    <>
      <section className="panel" data-verdict-panel style={{ marginTop: 18 }}>
        <h2>{t('nv.verdict')} <span className="note">{t('nv.verdictHelp')}</span></h2>
        {pending.length === 0
          ? <p className="dim">{t('nv.nonePending')}</p>
          : (
            <div className="tbl-wrap">
              <table className="tbl">
                <thead>
                  <tr>
                    <th>{t('col.agent')}</th>
                    <th>{t('col.role')}</th>
                    <th>{t('nv.candidate')}</th>
                    <th>{t('col.action')}</th>
                  </tr>
                </thead>
                <tbody>
                  {pending.map((e) => (
                    <PendingRow key={e.id} e={e} onDone={(message) => { setFlash(message); data.refresh(); }} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        {flash && <p role="status">{flash}</p>}
      </section>
      <section className="panel verdict-audit" style={{ marginTop: 18 }}>
        <h2>{t('nv.audit')} <span className="note">{t('nv.auditHelp')}</span></h2>
        {audit === null
          ? <p className="dim">…</p>
          : audit.length === 0
            ? <p className="dim">{t('nv.auditEmpty')}</p>
            : (
              <div className="tbl-wrap">
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>{t('nv.auditType')}</th>
                      <th>{t('nv.auditEngagement')}</th>
                      <th>{t('col.state')}</th>
                      <th>{t('nv.auditAt')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {audit.map((row, index) => (
                      <tr key={`${row.engagementId}-${row.at ?? index}-${index}`}>
                        <td className="mono-s">{row.type ?? '—'}</td>
                        <td className="mono-s dim">{row.engagementId}</td>
                        <td>{row.state}</td>
                        <td className="dim">{row.at === null || row.at === undefined ? t('nv.auditUnknown') : new Date(row.at).toISOString()}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
      </section>
    </>
  );
}
