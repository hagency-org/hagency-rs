'use client';
import { useEffect, useRef, useState } from 'react';
import { useData } from './Data';
import { useT } from './Prefs';
import { fetchAssociations, startAssociation } from '@/lib/native-api';

const DRAFT = 'hagency.setup.connection-draft';
const emptyForm = () => ({ homeserver: '', ownerMxid: '', coordinatorMxid: '', name: '', days: '90', palpoOrigin: '' });
const phases = ['awaiting_owner', 'awaiting_admin', 'awaiting_connection', 'connected'];

export default function NativeAssociation({ onConnected, onChange, selectedId, onSelect }) {
  const data = useData(), t = useT();
  const [rows, setRows] = useState([]), [form, setForm] = useState(null), [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const submitted = useRef(null), connected = useRef(new Set()), callbacks = useRef({});
  callbacks.current = { onConnected, onChange };
  const accessible = data.nativeConsole && ['ready', 'stale'].includes(data.phase);
  useEffect(() => {
    try {
      const saved = JSON.parse(sessionStorage.getItem(DRAFT));
      if (saved?.form && Object.keys(emptyForm()).every(key => typeof saved.form[key] === 'string')) {
        setForm(saved.form); setStep([0, 1, 2].includes(saved.step) ? saved.step : 0);
        submitted.current = saved.intent ?? null;
      }
    } catch { /* A malformed or unavailable local draft never blocks the console. */ }
  }, []);
  function observed(items) {
    setRows(items); callbacks.current.onChange?.(items);
    for (const row of items) {
      if (row.phase === 'connected' && !connected.current.has(row.id)) {
        connected.current.add(row.id); callbacks.current.onConnected?.();
      }
    }
  }
  useEffect(() => {
    if (!accessible) return;
    let active = true, timer;
    async function load() {
      try {
        const result = await fetchAssociations();
        if (!active) return;
        observed(result.associations); setError(old => old === 'load' ? '' : old);
      } catch {
        if (active) { setError('load'); callbacks.current.onChange?.([]); }
      } finally { if (active) timer = setTimeout(load, 3000); }
    }
    load();
    return () => { active = false; clearTimeout(timer); };
  }, [accessible]);
  function persist(value, next) {
    try { sessionStorage.setItem(DRAFT, JSON.stringify({ form: value, step: next, intent: submitted.current })); } catch {}
  }
  function open() {
    submitted.current = null; setError(''); setStep(0);
    const value = emptyForm(); setForm(value); persist(value, 0);
  }
  function change(key, value) {
    submitted.current = null;
    const next = { ...form, [key]: value }; setForm(next); persist(next, step);
  }
  function move(next) { setStep(next); persist(form, next); }
  function discard() {
    setForm(null); submitted.current = null; setError('');
    try { sessionStorage.removeItem(DRAFT); } catch {}
  }
  async function save(event) {
    event.preventDefault(); if (busy) return;
    if (step < 2) { move(step + 1); return; }
    setBusy(true); setError('');
    try {
      // Keep the id and deadline through a lost reply, reload, or language switch.
      // Only editing the input creates a new intent; the backend freezes each one.
      if (!submitted.current) submitted.current = {
        requestId: crypto.randomUUID(), homeserver: form.homeserver.trim(), name: form.name.trim(),
        ownerMxid: form.ownerMxid.trim(), coordinatorMxid: form.coordinatorMxid.trim(),
        delegationExpiresAtMs: Date.now() + Number(form.days) * 86400000,
        ...(form.palpoOrigin.trim() ? { palpoOrigin: form.palpoOrigin.trim() } : {}),
      };
      persist(form, step);
      await startAssociation(submitted.current);
      discard();
      // The command is saved even if this subsequent observation fails.
      try { observed((await fetchAssociations()).associations); } catch { setError('load'); }
    } catch { setError('start'); } finally { setBusy(false); }
  }
  if (!accessible) return null;
  return <section className="panel setup-stage" data-native-associations>
    <h2>{t('pa.title')}</h2><p>{t(form ? `guided.help.${step}` : 'pa.help')}</p>
    {error && <p role="alert">{t(`pa.error.${error}`)}</p>}
    {!form && <button className="btn primary" onClick={open}>{t('pa.new')}</button>}
    {form && <form className="association-form" onSubmit={save} data-association-form>
      <ol className="association-steps" aria-label={t('guided.connectionProgress')}>
        {['server', 'people', 'reviewSend'].map((name, i) => <li key={name} aria-current={step === i ? 'step' : undefined}>{i + 1}. {t(`guided.${name}`)}</li>)}
      </ol>
      <fieldset disabled={busy}>
        {step === 0 && <>
          <label>{t('pa.homeserver')}<input name="homeserver" type="url" required placeholder="https://matrix.example.org" value={form.homeserver} onChange={e => change('homeserver', e.target.value)} /><span className="field-help">{t('guided.serverHint')}</span></label>
          <label>{t('pa.name')}<input name="name" required maxLength={256} value={form.name} onChange={e => change('name', e.target.value)} /><span className="field-help">{t('guided.nameHint')}</span></label>
          <details><summary>{t('pa.advanced')}</summary><label>{t('pa.origin')}<input name="palpoOrigin" type="url" value={form.palpoOrigin} onChange={e => change('palpoOrigin', e.target.value)} /></label><p className="dim">{t('pa.originHelp')}</p></details>
        </>}
        {step === 1 && <>
          <label>{t('pa.owner')}<input name="ownerMxid" required maxLength={255} pattern="@[^: ]+:.+" placeholder="@owner:example.org" value={form.ownerMxid} onChange={e => change('ownerMxid', e.target.value)} /><span className="field-help">{t('guided.ownerHint')}</span></label>
          <label>{t('pa.coordinator')}<input name="coordinatorMxid" required maxLength={255} pattern="@[^: ]+:.+" placeholder="@coordinator:example.org" value={form.coordinatorMxid} onChange={e => change('coordinatorMxid', e.target.value)} /><span className="field-help">{t('guided.coordinatorHint')}</span></label>
          <label>{t('pa.duration')}<select name="days" value={form.days} onChange={e => change('days', e.target.value)}>
            {[30, 90, 365].map(days => <option key={days} value={days}>{t('pa.days', { days })}</option>)}
          </select><span className="field-help">{t('guided.durationHint')}</span></label>
        </>}
        {step === 2 && <>
          <h3>{t('guided.reviewConnection')}</h3>
          <dl className="association-review">{[['pa.name', form.name], ['pa.homeserver', form.homeserver], ['pa.owner', form.ownerMxid], ['pa.coordinator', form.coordinatorMxid], ['pa.duration', t('pa.days', { days: form.days })], ...(form.palpoOrigin ? [['pa.origin', form.palpoOrigin]] : [])].map(([key, value]) => <div key={key}><dt>{t(key)}</dt><dd>{value}</dd></div>)}</dl>
          <p className="note">{t('guided.beforeSend')}</p>
        </>}
        <div className="btn-row">
          <button type="submit" className="btn primary">{t(step < 2 ? 'guided.continue' : busy ? 'pa.sending' : 'pa.send')}</button>
          {step > 0 && <button type="button" className="btn" onClick={() => move(step - 1)}>{t('guided.back')}</button>}
          <button type="button" className="btn" onClick={discard}>{t('guided.discard')}</button>
        </div>
        <p className="dim">{t('guided.draft')}</p>
      </fieldset>
    </form>}
    <div aria-live="polite">{rows.map(row => <article key={row.id} className="association-status" data-association-id={row.id} data-association-phase={row.phase}>
      <div className="association-heading"><h3>{row.name}</h3>{onSelect && <button type="button" className="btn" aria-pressed={selectedId === row.id} disabled={error === 'load'} onClick={() => onSelect(row.id)}>{t(selectedId === row.id ? 'guided.selected' : 'guided.select')}</button>}</div>
      <p className="association-address">{row.homeserver}</p>
      <dl className="association-review"><div><dt>{t('pa.owner')}</dt><dd>{row.ownerMxid}</dd></div><div><dt>{t('pa.coordinator')}</dt><dd>{row.coordinatorMxid}</dd></div></dl>
      <p><strong>{t(`pa.phase.${row.phase}`)}</strong>{error === 'load' && ` · ${t('guided.lastObserved')}`}</p><p>{t(`pa.next.${row.phase}`)}</p>
      {phases.includes(row.phase) && <ol className="association-steps" aria-label={t('guided.connectionProgress')}>
        {phases.map((phase, i) => <li key={phase} className={phases.indexOf(row.phase) > i || row.phase === 'connected' ? 'complete' : ''} aria-current={phase === row.phase ? 'step' : undefined}>{t(`guided.phase.${phase}`)}</li>)}
      </ol>}
      {row.problem && <p role="status">{t(`pa.problem.${row.problem}`)}</p>}
      {row.imported && !row.transportEnabled && row.phase === 'awaiting_connection' && <p role="status">{t('pa.transportDisabled')}</p>}
      {['awaiting_owner', 'awaiting_admin', 'awaiting_connection', 'setup_failed'].includes(row.phase) && <a className="btn" href={`rinx://palpo/action/${row.actionId}`}>{t('pa.openRinx')}</a>}
      {row.phase === 'connected' && !onSelect && <a className="btn primary" href={`/console/resources/new/?server_engagement_id=${encodeURIComponent(row.id)}`}>{t('nc.pageTitle')}</a>}
      {row.phase === 'awaiting_owner' && <p className="dim">{t('pa.confirmBefore', { time: new Date(row.expiresAtMs).toLocaleString() })}</p>}
    </article>)}</div>
  </section>;
}
