'use client';
import { useEffect, useState } from 'react';
import { useData } from './Data';
import { useT } from './Prefs';
import { fetchServerEngagements, fetchEngagementResources, fetchCoordinatorDecisions, fetchAllocationResources, contributeEngagementResource, changeEngagementDelegation } from '@/lib/native-api';

const amount = value => Number.isSafeInteger(value) ? value.toLocaleString() : '—';
const instant = value => Number.isSafeInteger(value) ? new Date(value).toLocaleString() : '—';

export default function NativeServerEngagements(props) {
  const data = useData(); const t = useT();
  if (!data.nativeConsole || !['ready', 'stale'].includes(data.phase)) return <p role="status">{t('se.access')}</p>;
  return <EngagementList key={props.resourceId ?? 'all'} {...props} />;
}
function EngagementList({ resourceId = null, decisionsOnly = false }) {
  const t = useT();
  const [rows, setRows] = useState([]), [selected, setSelected] = useState(''), [next, setNext] = useState(null);
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    fetchServerEngagements().then(v => { if (active) { setRows(v.engagements); setNext(v.nextCursor); setSelected(v.engagements[0]?.id ?? ''); } }).catch(e => { if (active) setError(e.message); });
    return () => { active = false; };
  }, []);
  async function more() {
    if (busy) return; setBusy(true); setError('');
    try { const v = await fetchServerEngagements(next ?? ''); setRows(prior => [...prior, ...v.engagements]); setNext(v.nextCursor); }
    catch (e) { setError(e.message); } finally { setBusy(false); }
  }
  return <section className="panel" data-server-engagements>
    <h2>{t(decisionsOnly ? 'se.delivered' : 'nav.serverEngagements')}</h2>
    {error && <p role="alert">{error}</p>}
    <label>{t('se.choose')} <select value={selected} onChange={e => setSelected(e.target.value)}>
      <option value="">{t('se.choose')}</option>{rows.map(row => <option key={row.id} value={row.id}>{row.serverName} · {row.coordinatorMxid} · {row.id}</option>)}
    </select></label>
    {next && <button className="btn" disabled={busy} onClick={more}>{t('se.moreEngagements')}</button>}
    {!rows.length && <p>{t('se.empty')}</p>}
    {selected && <Engagement key={selected} row={rows.find(r => r.id === selected)} onChanged={v => setRows(old => old.map(r => r.id === v.id ? v : r))} resourceId={resourceId} decisionsOnly={decisionsOnly} />}
  </section>;
}
function Engagement({ row, onChanged, resourceId, decisionsOnly }) {
  const t = useT();
  const [grants, setGrants] = useState([]), [decisions, setDecisions] = useState([]), [choices, setChoices] = useState([]);
  const [grantNext, setGrantNext] = useState(null), [decisionNext, setDecisionNext] = useState(null), [resourceNext, setResourceNext] = useState(null);
  const [form, setForm] = useState(null), [busy, setBusy] = useState(false), [note, setNote] = useState('');
  useEffect(() => {
    let active = true;
    Promise.all([fetchCoordinatorDecisions(row.id), ...(decisionsOnly ? [] : [fetchEngagementResources(row.id), fetchAllocationResources()])]).then(([d, g, r]) => {
      if (!active) return;
      setDecisions(d.decisions); setDecisionNext(d.nextCursor);
      if (g) { setGrants(g.resources); setGrantNext(g.nextCursor); setChoices(r.resources); setResourceNext(r.next_after); }
    }).catch(e => { if (active) setNote(e.message); });
    return () => { active = false; };
  }, [row.id, decisionsOnly]);
  async function load(kind, reset = false) {
    if (busy) return; setBusy(true); setNote('');
    try {
      if (kind === 'decisions') { const v = await fetchCoordinatorDecisions(row.id, reset ? '' : decisionNext ?? ''); setDecisions(old => reset ? v.decisions : [...old, ...v.decisions]); setDecisionNext(v.nextCursor); }
      if (kind === 'grants') { const v = await fetchEngagementResources(row.id, reset ? '' : grantNext ?? ''); setGrants(old => reset ? v.resources : [...old, ...v.resources]); setGrantNext(v.nextCursor); }
      if (kind === 'resources') { const v = await fetchAllocationResources(resourceNext ?? ''); setChoices(old => [...old, ...v.resources]); setResourceNext(v.next_after); }
    } catch (e) { setNote(e.message); } finally { setBusy(false); }
  }
  function edit(grant) {
    setForm({ id: grant?.id ?? `grant_${crypto.randomUUID().replaceAll('-', '')}`, serverEngagementId: row.id,
      resourceId: grant?.resourceId ?? resourceId ?? choices[0]?.id ?? '', revision: (grant?.revision ?? 0) + 1,
      tokens: String(grant?.allocatedTokens ?? ''), managers: grant?.eligibleManagers.join('\n') ?? '' });
  }
  async function save(event) {
    event.preventDefault(); if (busy || !form) return;
    if (!/^[0-9]+$/.test(form.tokens) || !Number.isSafeInteger(Number(form.tokens))) { setNote(t('se.invalidAmount')); return; }
    setBusy(true); setNote('');
    try {
      const grant = { id: form.id, serverEngagementId: form.serverEngagementId, resourceId: form.resourceId, revision: form.revision,
        allocatedTokens: Number(form.tokens), eligibleManagers: [...new Set(form.managers.split(/[\s,]+/).filter(Boolean))] };
      await contributeEngagementResource(grant);
      const v = await fetchEngagementResources(row.id); setGrants(v.resources); setGrantNext(v.nextCursor); setForm(null); setNote(t('se.saved'));
    } catch (e) { setNote(e.message); } finally { setBusy(false); }
  }
  return <div>
    <p>{row.state} · {row.ownerMxid} · {t('se.coordinator')} {row.coordinatorMxid}</p>
    <p>{t('se.expires')} {instant(row.delegationExpiresAtMs)}</p>
    <p role="status">{note}</p>
    {!decisionsOnly && <>
      {!resourceId && <Delegation row={row} onChanged={onChanged} />}
      <h3>{t('se.capacity')}</h3><p>{t('se.capacityHelp')}</p>
      <div className="btn-row"><button className="btn" disabled={busy} onClick={() => load('grants', true)}>{t('common.refresh')}</button>
        <button className="btn primary" disabled={busy || row.state !== 'verified' || !choices.length} onClick={() => edit(null)}>{t('se.allocate')}</button></div>
      <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('se.resource')}</th><th>{t('se.period')}</th><th>{t('se.allocated')}</th><th>{t('se.retained')}</th><th>{t('se.available')}</th><th>{t('se.managers')}</th><th /></tr></thead>
        <tbody>{grants.filter(g => !resourceId || g.resourceId === resourceId).map(g => <tr key={g.id}><td>{g.resourceId}</td><td>{g.period} · {g.periodKey}</td><td>{amount(g.allocatedTokens)}</td><td>{amount(g.retainedTokens)}</td><td>{amount(g.remainingTokens)}</td><td>{g.eligibleManagers.join(', ')}</td><td><button className="btn" disabled={busy || row.state !== 'verified'} onClick={() => edit(g)}>{t('se.edit')}</button></td></tr>)}</tbody>
      </table></div>
      {grantNext && <button className="btn" disabled={busy} onClick={() => load('grants')}>{t('se.more')}</button>}
      {form && <form onSubmit={save} className="panel">
        <label>{t('se.resource')} <select required disabled={busy || form.revision > 1} value={form.resourceId} onChange={e => setForm({ ...form, resourceId: e.target.value })}>
          <option value="">{t('se.resource')}</option>{form.resourceId && !choices.some(r => r.id === form.resourceId) && <option value={form.resourceId}>{form.resourceId}</option>}{choices.map(r => <option key={r.id} value={r.id}>{r.model} · {r.id}</option>)}
        </select></label>
        {resourceNext && <button type="button" className="btn" disabled={busy} onClick={() => load('resources')}>{t('se.moreResources')}</button>}
        <label>{t('se.allocated')} <input required inputMode="numeric" value={form.tokens} disabled={busy} onChange={e => setForm({ ...form, tokens: e.target.value })} /></label>
        <label>{t('se.managers')} <textarea required value={form.managers} disabled={busy} onChange={e => setForm({ ...form, managers: e.target.value })} /></label>
        <button className="btn primary" disabled={busy}>{t('se.save')}</button><button type="button" className="btn" disabled={busy} onClick={() => setForm(null)}>{t('common.cancel')}</button>
      </form>}
    </>}
    {!decisionsOnly && <h3 style={{ marginTop: 24 }}>{t('se.delivered')}</h3>}<p>{t('se.deliveryHelp')}</p>
    <button className="btn" disabled={busy} onClick={() => load('decisions', true)}>{t('common.refresh')}</button>
    <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('col.agent')}</th><th>{t('se.project')}</th><th>{t('se.decidedBy')}</th><th>{t('se.approved')}</th><th>{t('col.state')}</th><th>{t('se.received')}</th></tr></thead>
      <tbody>{decisions.map(d => <tr key={d.id}><td>{d.agentName}<div className="dim">{d.agentAllocationId ?? d.requestId}</div></td><td>{d.projectId}<div className="dim">{d.projectOwner}</div></td><td>{d.decidedBy}<div className="dim">{instant(d.approvedAtMs)}</div></td><td>{amount(d.approvedTokens)}</td><td>{d.state}{d.reason && <p role="status">{d.reason}</p>}</td><td>{instant(d.receivedAtMs)}</td></tr>)}</tbody>
    </table></div>
    {!decisions.length && <p>{t('se.noDecisions')}</p>}
    {decisionNext && <button className="btn" disabled={busy} onClick={() => load('decisions')}>{t('se.more')}</button>}
  </div>;
}

function Delegation({ row, onChanged }) {
  const t = useT();
  const [form, setForm] = useState(null), [busy, setBusy] = useState(false), [note, setNote] = useState('');
  function edit() {
    setForm({ coordinator: row.coordinatorMxid, expires: new Date(row.delegationExpiresAtMs).toISOString().slice(0, 16),
      state: row.state === 'verified' ? 'active' : row.state, self: row.allowSelfApproval,
      ownerExport: row.exportMxids.includes(row.ownerMxid), coordinatorExport: row.exportMxids.includes(row.coordinatorMxid) });
    setNote('');
  }
  async function save(event) {
    event.preventDefault(); if (busy) return; setBusy(true); setNote('');
    try {
      const exports = [...new Set([...(form.ownerExport ? [row.ownerMxid] : []), ...(form.coordinatorExport ? [form.coordinator.trim()] : [])])];
      const change = { serverEngagementId: row.id, expectedRevision: row.delegationRevision, coordinatorMxid: form.coordinator.trim(),
        delegationExpiresAtMs: Date.parse(form.expires + 'Z'), allowSelfApproval: form.self, state: form.state, exportMxids: exports };
      const value = await changeEngagementDelegation(change), e = value.engagement;
      onChanged({ ...row, coordinatorMxid: e.coordinator, delegationRevision: e.delegationRevision, delegationExpiresAtMs: e.delegationExpiresAtMs,
        state: e.state, allowSelfApproval: e.allowSelfApproval, exportMxids: exports, delegationPublication: value.publication });
      setForm(null); setNote(t('se.delegationSaved'));
    } catch (e) { setNote(e.message); } finally { setBusy(false); }
  }
  return <section data-engagement-delegation>
    <h3>{t('se.delegation')}</h3><p>{t('se.delegationHelp')}</p>
    <p>{t('se.delegationRevision')} {row.delegationRevision} · {row.delegationPublication}</p>
    <p role="status">{note}</p>
    <button className="btn" disabled={busy || !['verified', 'suspended'].includes(row.state)} onClick={edit}>{t('se.editDelegation')}</button>
    {form && <form onSubmit={save} className="panel">
      <label>{t('se.coordinator')} <input required disabled={busy} value={form.coordinator} onChange={e => setForm({ ...form, coordinator: e.target.value })} /></label>
      <label>{t('se.expiresUtc')} <input type="datetime-local" required disabled={busy} value={form.expires} onChange={e => setForm({ ...form, expires: e.target.value })} /></label>
      <label>{t('col.state')} <select value={form.state} disabled={busy} onChange={e => setForm({ ...form, state: e.target.value })}>
        <option value="active">{t('se.active')}</option><option value="suspended">{t('se.suspended')}</option><option value="revoked">{t('se.revoked')}</option>
      </select></label>
      <label><input type="checkbox" checked={form.self} disabled={busy} onChange={e => setForm({ ...form, self: e.target.checked })} /> {t('se.selfApproval')}</label>
      <label><input type="checkbox" checked={form.ownerExport} disabled={busy} onChange={e => setForm({ ...form, ownerExport: e.target.checked })} /> {t('se.ownerExport')}</label>
      <label><input type="checkbox" checked={form.coordinatorExport} disabled={busy} onChange={e => setForm({ ...form, coordinatorExport: e.target.checked })} /> {t('se.coordinatorExport')}</label>
      <button className="btn primary" disabled={busy}>{t('se.saveDelegation')}</button><button type="button" className="btn" disabled={busy} onClick={() => setForm(null)}>{t('common.cancel')}</button>
    </form>}
  </section>;
}
