'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import PageHead from '@/components/PageHead';
import { useT } from '@/components/Prefs';
import { useData } from '@/components/Data';
import { errorText } from '@/lib/i18n';
import { fetchSetup, checkSetup, offerResource } from '@/lib/native-api';
import NativeAssociation from '@/components/NativeAssociation';

const STEPS = ['runtime', 'connection', 'resource', 'track'];
const AGENT_NAMES = { codex: 'Codex', claude: 'Claude Code', octos: 'Octos' };

function AgentCard({ agent }) {
  const t = useT();
  const name = AGENT_NAMES[agent.kind] ?? agent.kind;
  return <article className="setup-agent">
    <h3>{name}{agent.version ? ` · ${agent.version}` : ''}</h3>
    {!agent.found && <p>{t('st.notFound', { name })}</p>}
    {/* ADR-192 decision 6: Claude Code's sign-in is assumed, never checked;
        ADR-193 decision 8: so are Octos's keys. */}
    {agent.found && agent.signedIn && agent.signInAssumed && <p>{t(agent.kind === 'octos' ? 'st.octosKeys' : 'st.signInAssumed', { name })}</p>}
    {agent.kind === 'octos' && agent.profiles && <p>{t('st.octosProfiles')}</p>}
    {agent.kind === 'octos' && agent.profiles && <ul className="setup-profiles">
      {agent.profiles.map(p => <li key={p.id}><code>{p.id}</code> · {p.model ? `${p.family} / ${p.model}` : t('st.octosNoPrimary')} · {p.tier ? t('st.octosOffered', { tier: p.tier }) : t('st.octosNotQualified')}</li>)}
    </ul>}
    {agent.found && agent.signedIn && !agent.signInAssumed && <p>{t('st.signedIn', { kind: agent.signInKind === 'api_key' ? t('st.kindApiKey') : agent.signInKind === 'chatgpt' ? t('st.kindChatgpt') : '—' })}</p>}
    {agent.found && agent.signedIn && agent.signInKind === 'chatgpt' && <p className="note">{t('st.planNote')}</p>}
    {agent.found && !agent.signedIn && !agent.signInAssumed && <p>{t('st.notSignedIn', { name })} <code>{agent.kind === 'codex' ? 'codex login' : ''}</code></p>}
    {agent.problem && <p role="status">{agent.problem}</p>}
    {agent.path && <details><summary>{t('guided.runtimeDetails')}</summary><code>{agent.path}</code></details>}
  </article>;
}

export default function SetupPage() {
  const t = useT(), data = useData();
  const [setup, setSetup] = useState(null), [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false), [note, setNote] = useState(null);
  const [connections, setConnections] = useState([]), [selectedId, setSelectedId] = useState('');
  const [choiceIndex, setChoiceIndex] = useState(0), [tokens, setTokens] = useState('1000000');
  const loaded = useRef(false), autoCheck = useRef(false);
  const selected = connections.find(row => row.id === selectedId);
  const verified = selected?.phase === 'connected';
  const runtimeReady = !!setup?.runtimeConfigured && setup.agents?.some(agent => agent.found && agent.signedIn);
  const sourceReady = (setup?.offer?.sourceResources ?? setup?.offer?.resources ?? 0) > 0;
  // A coding agent configured after the first source (ADR-192: Claude Code
  // beside Codex) still gets its own offer: only agents with a source drop out.
  const sourceFrameworks = setup?.offer?.sourceFrameworks ?? [];
  const choices = (setup?.offer?.choices ?? []).filter(c => !sourceFrameworks.includes(c.framework ?? 'codex'));
  const choiceAt = choiceIndex < choices.length ? choiceIndex : 0, choice = choices[choiceAt];
  const load = useCallback(async () => {
    try {
      await data.ready;
      const value = await fetchSetup(); setSetup(value);
      if (!loaded.current) {
        loaded.current = true;
        let saved = 0;
        try { saved = Number(sessionStorage.getItem('hagency.setup.step')); } catch {}
        setStep(Number.isInteger(saved) && saved >= 0 && saved < STEPS.length ? saved : 0);
      }
    } catch (error) { setNote(errorText(t, error.message)); }
  }, [t, data.ready]);
  useEffect(() => { if (data.nativeConsole) load(); }, [data.nativeConsole, load]);
  async function check() {
    if (busy) return;
    setBusy(true); setNote(null);
    try {
      const value = await checkSetup(); setSetup(value);
      if (value.configured === false && value.problem) setNote(value.problem);
      else if (value.restartNeeded) setNote(t('st.restartNeeded'));
    } catch (error) {
      setNote(error.message === 'setup_not_fleet' ? t('st.notFleet') : errorText(t, error.message));
    } finally { setBusy(false); }
  }
  // Preserve the existing first-run configuration of an already signed-in runtime.
  useEffect(() => {
    if (!autoCheck.current && setup?.applicable && !setup.runtimeConfigured && setup.agents?.some(a => a.found && a.signedIn)) {
      autoCheck.current = true; check();
    }
  }, [setup]); // eslint-disable-line react-hooks/exhaustive-deps
  function move(next) { setStep(next); setNote(null); try { sessionStorage.setItem('hagency.setup.step', String(next)); } catch {} }
  function updateConnections(rows) {
    setConnections(rows);
    setSelectedId(old => rows.some(row => row.id === old) ? old : (rows.find(row => row.phase === 'connected') ?? rows[0])?.id ?? '');
  }
  async function prepareSource(event) {
    event.preventDefault(); if (busy || !choice) return;
    setBusy(true); setNote(null);
    try {
      await offerResource(choice.model, choice.reasoning, Number(tokens), choice.framework ?? 'codex', choice.profile ?? null);
      await load(); await data.refresh();
    } catch (error) {
      setNote(errorText(t, error.message));
      // A lost response or another tab may have created the source. Read the
      // observed state; never overwrite it by retrying an upsert.
      await load();
    } finally { setBusy(false); }
  }
  if (!data.nativeConsole) return <PageHead title={t('nav.setup')} sub={t('st.nativeOnly')} />;
  const completed = [!!runtimeReady, !!verified, false, false];
  return <>
    <PageHead title={t('guided.title')} sub={t('guided.sub')} />
    <nav className="setup-progress" aria-label={t('guided.progress')}>
      {STEPS.map((name, index) => <button key={name} type="button" aria-current={step === index ? 'step' : undefined} onClick={() => move(index)}>
        <span className={completed[index] ? 'complete' : ''}>{completed[index] ? <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true"><path d="m4 12 5 5L20 6" /></svg> : index + 1}</span>
        <b>{t(`guided.${name}`)}</b>{completed[index] && <small>{t(index === 0 ? 'guided.configured' : 'guided.verified')}</small>}
      </button>)}
    </nav>
    {note && <p role="status" className="note">{note}</p>}
    <div className={`setup-workspace${selected ? '' : ' without-context'}`}>
      <div>
        {step === 0 && <section className="panel setup-stage" aria-labelledby="runtime-title">
          <h2 id="runtime-title">{t('guided.runtimeTitle')}</h2><p>{t('guided.runtimeHelp')}</p>
          {setup === null ? <p role="status">{t('st.loading')}</p> : (setup.agents ?? []).map(agent => <AgentCard key={agent.kind} agent={agent} />)}
          <p role="status">{t(setup?.runtimeConfigured ? 'st.runtimeReady' : setup?.runtimeStale ? 'st.runtimeStale' : 'st.runtimeWaiting')}</p>
          {setup?.applicable === false && <p className="note">{t('st.notFleet')}</p>}
          <div className="btn-row"><button type="button" className="btn" disabled={busy} onClick={check}>{t(busy ? 'st.checking' : 'st.checkAgain')}</button><button type="button" className="btn primary" onClick={() => move(1)}>{t('guided.toConnection')}</button></div>
        </section>}
        {/* Keep polling the backend's association state when viewing another step. */}
        <div hidden={step !== 1}>
          <NativeAssociation onConnected={load} onChange={updateConnections} selectedId={selectedId} onSelect={setSelectedId} />
          {verified && <button className="btn primary" onClick={() => move(2)}>{t('guided.toResource')}</button>}
        </div>
        {step === 2 && <section className="panel setup-stage" aria-labelledby="resource-title">
          <h2 id="resource-title">{t('guided.resourceTitle')}</h2><p>{t('guided.resourceHelp')}</p>
          {!verified && <p className="note">{t('guided.connectionRequired')}</p>}
          {choices.length > 0 && <form className="setup-source-form" onSubmit={prepareSource}>
            <p>{t('guided.sourceHelp')}</p>
            <label>{t('guided.model')}<select value={choiceAt} onChange={e => setChoiceIndex(Number(e.target.value))} disabled={busy}>
              {choices.map((c, i) => <option key={`${c.framework}:${c.profile ?? ''}:${c.model}:${c.reasoning}`} value={i}>{AGENT_NAMES[c.framework] ?? 'Codex'}{c.profile ? ` · ${c.profile}` : ''} · {c.model}{c.reasoning ? ` · ${c.reasoning}` : ''}</option>)}
            </select></label>
            <label>{t('guided.ceiling')}<input type="number" min="1" max={Number.MAX_SAFE_INTEGER} step="1" required value={tokens} onChange={e => setTokens(e.target.value)} disabled={busy} /></label>
            <p className="dim">{t('guided.ceilingHelp')}</p>
            <button className="btn primary" disabled={busy || !choice || !runtimeReady || !Number.isSafeInteger(Number(tokens)) || Number(tokens) <= 0}>{t(busy ? 'guided.saving' : 'guided.prepareSource')}</button>
          </form>}
          {sourceReady && <><p className="note">{t('guided.sourceReady')}</p>{verified && <a className="btn primary" href={`/console/resources/new/?server_engagement_id=${encodeURIComponent(selected.id)}`}>{t('guided.allocate')}</a>}</>}
          <div className="btn-row"><button className="btn" onClick={() => move(1)}>{t('guided.back')}</button><button className="btn" onClick={() => move(3)}>{t('guided.toTracking')}</button></div>
        </section>}
        {step === 3 && <section className="panel setup-stage">
          <h2>{t('guided.trackTitle')}</h2><p>{t('guided.trackHelp')}</p>
          <ol className="setup-handoff">
            <li><b>{t('guided.resource')}</b><p>{t('guided.resourceNext')}</p><a className="btn" href="/console/resources/">{t('guided.openResources')}</a></li>
            <li><b>{t('guided.project')}</b><p>{t('guided.projectNext')}</p></li>
            <li><b>{t('guided.agent')}</b><p>{t('guided.agentNext')}</p><a className="btn" href="/console/server-engagements/">{t('guided.openEngagements')}</a></li>
          </ol>
          <p className="note">{t('guided.truth')}</p>
        </section>}
      </div>
      {selected && <aside className="panel setup-context">
        <h2>{t('guided.context')}</h2>
        <dl><dt>{t('guided.host')}</dt><dd>{t('guided.thisHost')}</dd>
          <dt>{t('pa.homeserver')}</dt><dd>{selected?.homeserver ?? t('guided.notSelected')}</dd>
          <dt>{t('pa.owner')}</dt><dd>{selected?.ownerMxid ?? '—'}</dd>
          <dt>{t('pa.coordinator')}</dt><dd>{selected?.coordinatorMxid ?? '—'}</dd>
          <dt>{t('guided.connectionStatus')}</dt><dd>{selected ? t(`pa.phase.${selected.phase}`) : t('guided.notRequested')}</dd></dl>
        <p className="dim">{t('guided.persist')}</p>
      </aside>}
    </div>
  </>;
}
