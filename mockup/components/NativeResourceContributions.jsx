'use client';
import { useEffect, useRef, useState } from 'react';
import { useT } from '@/components/Prefs';
import { contributeResource, fetchContributionTargets, fetchResourceContributions, revokeResourceContribution } from '@/lib/native-api';

const emptyForm = { fleet: '', tokens: '', agents: '', rate: '', expiry: '' };
const localDate = ms => new Date(ms - new Date(ms).getTimezoneOffset() * 60000).toISOString().slice(0, 16);
export default function NativeResourceContributions({ resource, allowed, refreshResource }) {
  const t = useT();
  const [form, setForm] = useState(emptyForm), [page, setPage] = useState(null), [sides, setSides] = useState([]), [nextSide, setNextSide] = useState(null);
  const [pending, setPending] = useState(null), [busy, setBusy] = useState(false), [error, setError] = useState(null), [notice, setNotice] = useState(null);
  const mounted = useRef(false), revision = useRef(0), writing = useRef(false);
  const draftKey = `hagency:contribution:${resource.id}`;
  const load = async (after = '') => {
    const turn = ++revision.current;
    try {
      const [rows, connections] = await Promise.all([fetchResourceContributions(resource.id, after), fetchContributionTargets()]);
      if (!mounted.current || turn !== revision.current) return;
      setPage(rows); setSides(connections.targets.filter(s => s.receptionBound)); setNextSide(connections.nextAfter);
    } catch (e) { if (mounted.current && turn === revision.current) setError(e.message); }
  };
  useEffect(() => {
    mounted.current = true;
    try {
      const saved = JSON.parse(sessionStorage.getItem(draftKey) ?? 'null');
      if (saved && /^[A-Za-z0-9_-]{1,96}$/.test(saved.requestId) && saved.limits && Number.isSafeInteger(saved.expiresAtMs)) {
        setPending(saved);
        setForm({ fleet: saved.fleetId, tokens: String(saved.limits.tokens), agents: String(saved.limits.maxAgents), rate: String(saved.limits.maxRatePerDay), expiry: localDate(saved.expiresAtMs) });
        setNotice('pc.recovered');
      }
    } catch { setError('draft_unavailable'); }
    load();
    return () => { mounted.current = false; revision.current++; };
  }, [draftKey]);
  const moreTargets = async () => {
    const turn = revision.current;
    try {
      const result = await fetchContributionTargets(nextSide);
      if (mounted.current && turn === revision.current) { setSides(old => [...old, ...result.targets.filter(s => s.receptionBound && !old.some(r => r.fleetId === s.fleetId))]); setNextSide(result.nextAfter); }
    } catch (e) { if (mounted.current) setError(e.message); }
  };
  const change = key => e => setForm(old => ({ ...old, [key]: e.target.value }));
  const send = async e => {
    e.preventDefault();
    if (writing.current || !allowed) return;
    const side = sides.find(s => s.fleetId === form.fleet);
    const limits = { tokens: Number(form.tokens), maxAgents: Number(form.agents), maxRatePerDay: Number(form.rate) };
    const expiresAtMs = new Date(form.expiry).getTime();
    if (!pending && (!side || !Object.values(limits).every(n => Number.isSafeInteger(n) && n > 0) || limits.maxAgents > 10000 || !Number.isSafeInteger(expiresAtMs) || expiresAtMs <= Date.now())) { setError('invalid_limits'); return; }
    const input = pending ?? { requestId: crypto.randomUUID(), expectedResourceRevision: resource.revision, fleetId: side.fleetId, registrationGeneration: side.registrationGeneration, limits, expiresAtMs };
    // Exact retry data only: never store credentials or observed usage here.
    try { sessionStorage.setItem(draftKey, JSON.stringify(input)); } catch { setError('draft_unavailable'); return; }
    setPending(input); writing.current = true; setBusy(true); setError(null); setNotice(null);
    try {
      await contributeResource(resource.id, input);
      sessionStorage.removeItem(draftKey);
      if (mounted.current) { setPending(null); setForm(emptyForm); setNotice('pc.saved'); await load(); refreshResource(); }
    } catch (e) { if (mounted.current) setError(e.message); }
    finally { writing.current = false; if (mounted.current) setBusy(false); }
  };
  const remove = async row => {
    if (!allowed || writing.current || !window.confirm(t('pc.revokeConfirm'))) return;
    writing.current = true; setBusy(true); setError(null); setNotice(null);
    try { await revokeResourceContribution(resource, row); if (mounted.current) { setNotice('pc.revoked'); await load(); refreshResource(); } }
    catch (e) { if (mounted.current) setError(e.message); }
    finally { writing.current = false; if (mounted.current) setBusy(false); }
  };
  const reset = () => {
    try { sessionStorage.removeItem(draftKey); } catch { setError('draft_unavailable'); return; }
    setPending(null); setError(null); setNotice(null);
  };
  const editableRefusal = ['invalid_limits', 'invalid_console_request', 'invalid_resource_command', 'resource_revision_conflict', 'insufficient_capacity', 'connection_verification_required', 'contribution_expired', 'contribution_revoked'].includes(error);
  return <section className="panel" data-native-contributions>
    <h2 className="sec" style={{ marginTop: 0 }}>{t('pc.title')}</h2><p>{t('pc.help')}</p>
    {notice && <p role="status">{t(notice)}</p>}
    {error && <p role="alert">{t(`pc.error.${error}`) === `pc.error.${error}` ? t('pc.error.generic') : t(`pc.error.${error}`)}</p>}
    {!page && !error && <p role="status">{t('pc.loading')}</p>}
    {page && <>
      {page.contributions.length === 0 ? <p className="dim">{t('pc.empty')}</p> : <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('pc.connection')}</th><th>{t('pc.budget')}</th><th>{t('pc.reserved')}</th><th>{t('pc.expiry')}</th><th>{t('pc.state')}</th><th>{t('col.action')}</th></tr></thead><tbody>
        {page.contributions.map(row => <tr key={row.grant.id} data-contribution-id={row.grant.id}><td>{row.grant.issuer}<div className="dim">{row.grant.fleetId.slice(-8)}</div></td><td>{row.grant.limits.tokens.toLocaleString()}<div className="dim">{t('pc.agents')}: {row.grant.limits.maxAgents} · {t('pc.rate')}: {row.grant.limits.maxRatePerDay.toLocaleString()}</div></td><td>{row.reserved.tokens.toLocaleString()}</td><td>{new Date(row.grant.expiresAtMs).toLocaleString()}</td><td>{t(`pc.state.${row.state}`)}</td><td><button className="btn" disabled={!allowed || busy || row.state === 'revoked'} onClick={() => remove(row)}>{t('pc.revoke')}</button></td></tr>)}
      </tbody></table></div>}
      <div className="btn-row"><button className="btn" disabled={busy} onClick={() => { setError(null); load(); }}>{t('nu.refresh')}</button><button className="btn" disabled={busy || !page.nextAfter} onClick={() => load(page.nextAfter)}>{t('nu.nextPage')}</button></div>
    </>}
    {allowed && <form onSubmit={send} style={{ marginTop: 20 }}>
      <h3>{t('pc.create')}</h3>
      {!sides.length && <p>{t('pc.noConnection')} <a href="/console/setup/">{t('pc.setup')}</a></p>}
      <fieldset className="contribution-fields" disabled={busy || !!pending}><div className="field"><label htmlFor="contribution-fleet">{t('pc.connection')}</label><select id="contribution-fleet" value={form.fleet} onChange={change('fleet')} required><option value="">{t('pc.choose')}</option>{pending && !sides.some(s => s.fleetId === form.fleet) && <option value={form.fleet}>{t('pc.savedConnection')} · {form.fleet.slice(-8)}</option>}{sides.map(s => <option key={s.fleetId} value={s.fleetId}>{s.issuer} · {s.fleetId.slice(-8)}</option>)}</select>{nextSide && <button type="button" className="btn" onClick={moreTargets}>{t('pc.moreConnections')}</button>}</div>
        <div className="field"><label htmlFor="contribution-expiry">{t('pc.expiryLocal')}</label><input id="contribution-expiry" type="datetime-local" value={form.expiry} onChange={change('expiry')} required /></div>
        <div className="contribution-limits">{[['tokens', 'pc.budget'], ['agents', 'pc.agents'], ['rate', 'pc.rate']].map(([key, title]) => <div className="field" key={key}><label htmlFor={`contribution-${key}`}>{t(title)}</label><input id={`contribution-${key}`} type="number" min="1" max={key === 'agents' ? 10000 : Number.MAX_SAFE_INTEGER} step="1" value={form[key]} onChange={change(key)} required /></div>)}</div>
      </fieldset>
      <p className="note">{t('pc.terms')}</p><div className="btn-row"><button className="btn primary" disabled={busy || (!pending && !sides.length)}>{t(busy ? 'pc.saving' : pending ? 'pc.retry' : 'pc.submit')}</button>{pending && editableRefusal && <button className="btn" type="button" disabled={busy} onClick={reset}>{t('pc.edit')}</button>}</div>
    </form>}
  </section>;
}
