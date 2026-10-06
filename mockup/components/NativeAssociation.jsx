'use client';
import { useEffect, useRef, useState } from 'react';
import { useData } from './Data';
import { useT } from './Prefs';
import { fetchAssociations, startAssociation } from '@/lib/native-api';

export default function NativeAssociation({ onConnected }) {
  const data = useData(), t = useT();
  const [rows, setRows] = useState([]), [form, setForm] = useState(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const submitted = useRef(null), connected = useRef(new Set()), callback = useRef(onConnected);
  callback.current = onConnected;
  const accessible = data.nativeConsole && ['ready', 'stale'].includes(data.phase);
  useEffect(() => {
    if (!accessible) return;
    let active = true, timer;
    async function load() {
      try {
        const result = await fetchAssociations();
        if (!active) return;
        setRows(result.associations); setError(old => old === 'load' ? '' : old);
        for (const row of result.associations) {
          if (row.phase === 'connected' && !connected.current.has(row.id)) {
            connected.current.add(row.id); callback.current?.();
          }
        }
      } catch { if (active) setError('load'); }
      finally { if (active) timer = setTimeout(load, 3000); }
    }
    load();
    return () => { active = false; clearTimeout(timer); };
  }, [accessible]);
  function open() {
    submitted.current = null; setError('');
    setForm({ homeserver: '', ownerMxid: '', coordinatorMxid: '', name: '', days: '90', palpoOrigin: '' });
  }
  function change(key, value) {
    submitted.current = null;
    setForm(old => ({ ...old, [key]: value }));
  }
  async function save(event) {
    event.preventDefault(); if (busy) return;
    setBusy(true); setError('');
    try {
      // Retrying a lost reply reuses the same request and expiry. Editing the
      // form starts a distinct intent; the server freezes each submitted one.
      if (!submitted.current) submitted.current = {
        requestId: crypto.randomUUID(), homeserver: form.homeserver.trim(), name: form.name.trim(),
        ownerMxid: form.ownerMxid.trim(), coordinatorMxid: form.coordinatorMxid.trim(),
        delegationExpiresAtMs: Date.now() + Number(form.days) * 86400000,
        ...(form.palpoOrigin.trim() ? { palpoOrigin: form.palpoOrigin.trim() } : {}),
      };
      await startAssociation(submitted.current);
      setForm(null); submitted.current = null;
      setRows((await fetchAssociations()).associations);
    } catch { setError('start'); } finally { setBusy(false); }
  }
  if (!accessible) return null;
  return <section className="panel" data-native-associations>
    <h2>{t('pa.title')}</h2><p>{t('pa.help')}</p>
    {error && <p role="alert">{t(`pa.error.${error}`)}</p>}
    {!form && <button className="btn primary" onClick={open}>{t('pa.new')}</button>}
    {form && <form onSubmit={save} data-association-form>
      <fieldset disabled={busy} style={{ border: 0, padding: 0, display: 'grid', gridTemplateColumns: 'minmax(0, 1fr)', alignItems: 'start', background: 'transparent', gap: 12, maxWidth: 580 }}>
        <label>{t('pa.homeserver')}<input name="homeserver" type="url" required placeholder="https://matrix.example.org" value={form.homeserver} onChange={e => change('homeserver', e.target.value)} style={{ width: '100%' }} /></label>
        <label>{t('pa.owner')}<input name="ownerMxid" required maxLength={255} placeholder="@owner:example.org" value={form.ownerMxid} onChange={e => change('ownerMxid', e.target.value)} style={{ width: '100%' }} /></label>
        <label>{t('pa.coordinator')}<input name="coordinatorMxid" required maxLength={255} placeholder="@coordinator:example.org" value={form.coordinatorMxid} onChange={e => change('coordinatorMxid', e.target.value)} style={{ width: '100%' }} /></label>
        <p className="dim">{t('pa.accountsHelp')}</p>
        <label>{t('pa.name')}<input name="name" required maxLength={256} value={form.name} onChange={e => change('name', e.target.value)} style={{ width: '100%' }} /></label>
        <label>{t('pa.duration')} <select name="days" value={form.days} onChange={e => change('days', e.target.value)}>
          {[30, 90, 365].map(days => <option key={days} value={days}>{t('pa.days', { days })}</option>)}
        </select></label>
        <details><summary>{t('pa.advanced')}</summary><label>{t('pa.origin')}<input name="palpoOrigin" type="url" value={form.palpoOrigin} onChange={e => change('palpoOrigin', e.target.value)} style={{ width: '100%' }} /></label><p className="dim">{t('pa.originHelp')}</p></details>
        <div className="btn-row"><button type="submit" className="btn primary">{t(busy ? 'pa.sending' : 'pa.send')}</button><button type="button" className="btn" onClick={() => setForm(null)}>{t('wz.cancel')}</button></div>
      </fieldset>
    </form>}
    <div aria-live="polite">{rows.map(row => <article key={row.id} data-association-id={row.id} data-association-phase={row.phase} style={{ marginTop: 20 }}>
      <h3>{row.name}</h3><p>{row.homeserver} · {row.ownerMxid} → {row.coordinatorMxid}</p>
      <p><strong>{t(`pa.phase.${row.phase}`)}</strong></p><p>{t(`pa.next.${row.phase}`)}</p>
      {row.problem && <p role="status">{t(`pa.problem.${row.problem}`)}</p>}
      {row.imported && !row.transportEnabled && row.phase === 'awaiting_connection' && <p role="status">{t('pa.transportDisabled')}</p>}
      {['awaiting_owner', 'awaiting_admin', 'awaiting_connection', 'setup_failed'].includes(row.phase) && <a className="btn" href={`rinx://palpo/action/${row.actionId}`}>{t('pa.openRinx')}</a>}
      {row.phase === 'connected' && <a className="btn primary" href="/console/resources/new/">{t('nc.pageTitle')}</a>}
      {row.phase === 'awaiting_owner' && <p className="dim">{t('pa.confirmBefore', { time: new Date(row.expiresAtMs).toLocaleString() })}</p>}
    </article>)}</div>
  </section>;
}
