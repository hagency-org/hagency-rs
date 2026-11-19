'use client';

import { useEffect, useState } from 'react';
import PageHead from '@/components/PageHead';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';
import { errorText } from '@/lib/i18n';
import { fetchPluginLogs, fetchPluginManifest, fetchPluginQualifications, fetchPluginSettings, fetchPluginTools, fetchPlugins, installPlugin, invokePluginAction, linkPlugin, previewPlugin, setPluginEnabled, uninstallPlugin, updatePlugin, updatePluginSettings } from '@/lib/native-api';

const emptySource = { owner: '', repo: '', subdir: '', commit: '', transport: 'https' };
const sameSourceFamily = (installed, previewed) => installed?.kind === 'github' && previewed?.kind === 'github'
  && installed.owner === previewed.owner && installed.repo === previewed.repo
  && (installed.subdir ?? '') === (previewed.subdir ?? '') && installed.transport === previewed.transport;

export default function NativePlugins() {
  const t = useT();
  const data = useData();
  const [plugins, setPlugins] = useState([]);
  const [path, setPath] = useState('');
  const [source, setSource] = useState(emptySource);
  const [preview, setPreview] = useState(null);
  const [phase, setPhase] = useState('loading');
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(null);
  const [details, setDetails] = useState(null);
  const [draftSettings, setDraftSettings] = useState({});

  async function load() {
    try {
      setPhase('loading');
      setPlugins(await fetchPlugins());
      setPhase('ready');
      setError(null);
    } catch (cause) {
      setPhase('error');
      setError(cause.message);
    }
  }
  useEffect(() => {
    let stopped = false;
    data.ready.then(() => { if (!stopped) void load(); });
    return () => { stopped = true; };
  }, [data.ready]);

  async function operation(key, fn) {
    setBusy(key); setError(null);
    try { await fn(); setDetails(null); await load(); return true; }
    catch (cause) { setError(cause.message); return false; }
    finally { setBusy(null); }
  }
  async function inspectPlugin(id) {
    setBusy(`inspect:${id}`); setError(null);
    try {
      const [manifest, qualifications, logs, settings, tools] = await Promise.all([
        fetchPluginManifest(id), fetchPluginQualifications(id), fetchPluginLogs(id), fetchPluginSettings(id), fetchPluginTools(id),
      ]);
      setDraftSettings(settings.document.values);
      setDetails({ id, manifest, qualifications, logs, settings, tools });
    } catch (cause) { setError(cause.message); }
    finally { setBusy(null); }
  }
  async function refreshLogs() {
    if (!details) return;
    setBusy(`logs:${details.id}`); setError(null);
    try {
      const logs = await fetchPluginLogs(details.id);
      setDetails((old) => ({ ...old, logs }));
    } catch (cause) { setError(cause.message); }
    finally { setBusy(null); }
  }
  async function invoke(id, actionId) {
    setBusy(`action:${actionId}`); setError(null);
    try { await invokePluginAction(id, actionId, {}); await inspectPlugin(id); }
    catch (cause) { setError(cause.message); setBusy(null); }
  }
  async function saveSettings() {
    if (!details?.settings) return;
    setBusy(`settings:${details.id}`); setError(null);
    try {
      const settings = await updatePluginSettings(details.id, details.settings.document.revision, draftSettings);
      setDetails((old) => ({ ...old, settings }));
    } catch (cause) { setError(cause.message); }
    finally { setBusy(null); }
  }
  async function link() {
    if (!path.trim().startsWith('/')) return;
    await operation('link', async () => { await linkPlugin(path.trim()); setPath(''); });
  }
  async function previewSource(event) {
    event.preventDefault();
    setBusy('preview'); setError(null);
    try {
      const next = {
        kind: 'github',
        owner: source.owner.trim(),
        repo: source.repo.trim(),
        subdir: source.subdir.trim() || null,
        commit: source.commit.trim(),
        transport: source.transport,
      };
      if (!/^[a-f0-9]{40}$/.test(next.commit) || !next.owner || !next.repo) throw new Error('invalid_selection');
      setPreview(await previewPlugin(next));
    } catch (cause) { setError(cause.message); }
    finally { setBusy(null); }
  }
  async function install() {
    if (!preview) return;
    if (await operation('install', () => installPlugin(preview.source, preview.trustSha256))) {
      setPreview(null);
      setSource(emptySource);
    }
  }
  async function update() {
    if (!preview || !previewTarget) return;
    if (await operation('update', () => updatePlugin(preview.source, preview.trustSha256))) {
      setPreview(null);
      setSource(emptySource);
    }
  }
  const errorMessage = errorText(t, error);
  const linkReady = path.trim().startsWith('/');
  const remoteReady = /^[a-f0-9]{40}$/.test(source.commit.trim());
  const previewTarget = preview && plugins.find((plugin) => plugin.id === preview.manifest.id && sameSourceFamily(plugin.source, preview.source));
  return <>
    <PageHead title={t('pl.title')} sub={t('pl.sub')} />
    <section className="panel plugin-model" aria-labelledby="plugin-model" data-plugin-model>
      <h2 id="plugin-model" className="sec">{t('pl.modelTitle')}</h2>
      <p className="dim">{t('pl.modelHelp')}</p>
      <div className="rolegrid">
        <article className="rolecard"><h3>{t('pl.platformTitle')}</h3><p>{t('pl.platformHelp')}</p></article>
        <article className="rolecard"><h3>{t('pl.harnessTitle')}</h3><p>{t('pl.harnessHelp')}</p></article>
        <article className="rolecard"><h3>{t('pl.runtimeTitle')}</h3><p>{t('pl.runtimeHelp')}</p></article>
      </div>
      <p className="notice">{t('pl.targetModel')}</p>
      <p className="dim">{t('pl.modelStatus')}</p>
    </section>
    <section className="panel plugin-install" aria-labelledby="plugin-link">
      <h2 id="plugin-link" className="sec">{t('pl.installTitle')}</h2>
      <p className="dim">{t('pl.installHelp')}</p>
      <div className="plugin-link-row">
        <label className="grow">{t('pl.path')}<input value={path} onChange={(event) => setPath(event.target.value)} placeholder={t('pl.pathPlaceholder')} /></label>
        <button className="btn" disabled={!linkReady || busy !== null} onClick={link}>{busy === 'link' ? '…' : t('pl.link')}</button>
      </div>
      <form className="plugin-remote-form" onSubmit={previewSource}>
        <fieldset className="plugin-source-grid">
          <legend>{t('pl.remoteTitle')}</legend>
          <p className="dim plugin-source-help">{t('pl.remoteHelp')}</p>
          <label>{t('pl.owner')}<input value={source.owner} onChange={(event) => setSource((old) => ({ ...old, owner: event.target.value }))} autoComplete="off" /></label>
          <label>{t('pl.repo')}<input value={source.repo} onChange={(event) => setSource((old) => ({ ...old, repo: event.target.value }))} autoComplete="off" /></label>
          <label>{t('pl.subdir')}<input value={source.subdir} onChange={(event) => setSource((old) => ({ ...old, subdir: event.target.value }))} placeholder={t('pl.subdirOptional')} autoComplete="off" /></label>
          <label>{t('pl.commit')}<input className="mono-s" value={source.commit} onChange={(event) => setSource((old) => ({ ...old, commit: event.target.value }))} placeholder={t('pl.commitPlaceholder')} autoComplete="off" /></label>
          <label>{t('pl.transport')}<select value={source.transport} onChange={(event) => setSource((old) => ({ ...old, transport: event.target.value }))}><option value="https">HTTPS</option><option value="ssh">SSH</option></select></label>
          <button className="btn primary" type="submit" disabled={!source.owner.trim() || !source.repo.trim() || !remoteReady || busy !== null}>{busy === 'preview' ? '…' : t('pl.preview')}</button>
        </fieldset>
      </form>
      {preview && <div className="notice plugin-preview" role="status" data-plugin-preview>
        <div className="plugin-preview-head"><div><b>{preview.manifest.name}</b><span className="dim"> · {preview.manifest.id} · {preview.manifest.version}</span></div><span className="plugin-source-badge">{preview.source.transport.toUpperCase()}</span></div>
        <dl className="plugin-digests">
          <div><dt>{t('pl.sourceDigest')}</dt><dd className="mono-s">{preview.sourceSha256}</dd></div>
          <div><dt>{t('pl.manifestDigest')}</dt><dd className="mono-s">{preview.manifestSha256}</dd></div>
          <div><dt>{t('pl.trustDigest')}</dt><dd className="mono-s">{preview.trustSha256}</dd></div>
        </dl>
        <p className="dim">{t('pl.previewSummary', { files: preview.files.length, builds: preview.buildArgv.length })}</p>
        {preview.buildArgv.length > 0 && <details className="plugin-builds"><summary>{t('pl.buildCommands')}</summary><ul>{preview.buildArgv.map((argv, index) => <li key={index}><code>{argv.join(' ')}</code></li>)}</ul></details>}
        <p className="plugin-trust-note">{previewTarget ? t('pl.updateHelp', { id: previewTarget.id }) : t('pl.trustHelp')}</p>
        <button className="btn primary" disabled={busy !== null} data-plugin-update={previewTarget ? 'true' : undefined} onClick={previewTarget ? update : install}>{busy === (previewTarget ? 'update' : 'install') ? '…' : previewTarget ? t('pl.update') : t('pl.install')}</button>
      </div>}
    </section>
    <section aria-labelledby="plugin-list">
      <h2 id="plugin-list" className="sec">{t('pl.title')}</h2>
      {phase === 'loading' && <p role="status">{t('nr.loading')}</p>}
      {error && <p className="notice bad" role="alert">{t('pl.failed', { error: errorMessage })}</p>}
      {phase === 'ready' && plugins.length === 0 && <p className="empty">{t('pl.empty')}</p>}
      {plugins.length > 0 && <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('pl.id')}</th><th>{t('col.version')}</th><th>{t('pl.status')}</th><th>{t('pl.revisions')}</th><th>{t('pl.digest')}</th><th /></tr></thead><tbody>{plugins.map((plugin) => <tr key={plugin.id}>
        <td className="mono-s" data-token={plugin.id}>{plugin.id}</td><td>{plugin.manifestVersion}</td><td>{plugin.readiness}{plugin.stale ? ` · ${t('pl.stale')}` : ''}{plugin.restartRequired ? ` · ${t('pl.restart')}` : ''}</td><td>{Object.keys(plugin.revisions ?? {}).length} / {plugin.activeLeases ?? 0}</td><td className="mono-s" data-token={plugin.activePackageSha256}>{plugin.activePackageSha256}</td>
        <td className="btn-row">
          <button className="btn" disabled={busy !== null} onClick={() => inspectPlugin(plugin.id)}>{busy === `inspect:${plugin.id}` ? '…' : t('pl.inspect')}</button>
          {plugin.enabled
            ? <button className="btn" disabled={busy !== null} onClick={() => operation(plugin.id, () => setPluginEnabled(plugin.id, false))}>{t('pl.disable')}</button>
            : <button className="btn primary" disabled={busy !== null} onClick={() => operation(plugin.id, () => setPluginEnabled(plugin.id, true))}>{t('pl.enable')}</button>}
          <button className="btn danger" disabled={busy !== null || plugin.enabled} onClick={() => operation(plugin.id, () => uninstallPlugin(plugin.id))}>{t('pl.uninstall')}</button>
        </td>
      </tr>)}</tbody></table></div>}
    </section>
    {details && <section className="panel plugin-details" aria-label={t('pl.details')}>
      <div className="btn-row plugin-detail-head"><h2 className="sec grow plugin-detail-title">{details.manifest.name} · {details.id}</h2><button className="btn" disabled={busy !== null} onClick={refreshLogs}>{busy === `logs:${details.id}` ? '…' : t('pl.refresh')}</button></div>
      <p className="mono-s">{t('pl.package')} {plugins.find((plugin) => plugin.id === details.id)?.packageSha256}</p>
      <h3>{t('pl.tools')}</h3>
      {details.tools.length === 0 ? <p className="dim">{t('pl.none')}</p> : <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('pl.tool')}</th><th>{t('pl.description')}</th><th>{t('pl.inputSchema')}</th><th>{t('pl.package')}</th></tr></thead><tbody>{details.tools.map((tool) => <tr key={tool.toolId}><td><code>{tool.name}</code></td><td>{tool.description}</td><td><pre className="cmd">{JSON.stringify(tool.inputSchema)}</pre></td><td className="mono-s">{tool.packageSha256}</td></tr>)}</tbody></table></div>}
      <h3>{t('pl.runtimeAdapters')}</h3>
      {details.manifest.runtimes.length === 0 ? <p className="dim">{t('pl.none')}</p> : <ul>{details.manifest.runtimes.map((runtime) => <li key={runtime.id}><code>{runtime.id}</code> · {runtime.framework} · {runtime.protocol}</li>)}</ul>}
      <h3>{t('pl.harnessAdapters')}</h3>
      {details.manifest.harnesses.length === 0 ? <p className="dim">{t('pl.none')}</p> : <ul>{details.manifest.harnesses.map((harness) => <li key={harness.id}><code>{harness.id}</code> · {harness.protocol}</li>)}</ul>}
      <h3>{t('pl.qualifiedChoices')}</h3>
      {details.qualifications.length === 0 ? <p className="dim">{t('pl.noQualified')}</p> : <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('pl.framework')}</th><th>{t('pl.model')}</th><th>{t('pl.provider')}</th><th>{t('pl.reasoning')}</th><th>{t('pl.roles')}</th></tr></thead><tbody>{details.qualifications.map((choice) => <tr key={`${choice.adapterId}-${choice.framework}-${choice.model}-${choice.reasoning ?? ''}`}><td>{choice.framework}</td><td className="mono-s">{choice.model}</td><td>{choice.provider}</td><td>{choice.reasoning ?? t('pl.default')}</td><td>{choice.roles.join(', ')}</td></tr>)}</tbody></table></div>}
      <h3>{t('pl.activity')}</h3>
      {details.logs.length === 0 ? <p className="dim">{t('pl.noActivity')}</p> : <pre className="cmd">{details.logs.map((entry) => `[${new Date(entry.atMs).toISOString()}] ${entry.stream}\n${entry.text}`).join('\n')}</pre>}
      <h3>{t('pl.settings')}</h3>
      <p className="dim">{t('pl.revisionState', { revision: details.settings.document.revision, state: details.settings.settingsReady ? t('pl.ready') : t('pl.missing', { values: details.settings.missingRequired.join(', ') }) })}</p>
      {details.settings.definitions.length === 0 ? <p className="dim">{t('pl.noSettings')}</p> : <div className="plugin-settings-grid">{details.settings.definitions.map((setting) => {
        const value = Object.hasOwn(draftSettings, setting.id) ? draftSettings[setting.id] : (details.settings.document.values[setting.id] ?? setting.default ?? (setting.kind === 'boolean' ? false : ''));
        const update = (next) => setDraftSettings((old) => ({ ...old, [setting.id]: next }));
        return <label key={setting.id} className="plugin-setting"><span>{setting.label}{setting.required ? ' *' : ''}</span><small className="dim">{setting.description}</small>{setting.kind === 'boolean'
          ? <input className="plugin-setting-toggle" type="checkbox" checked={value === true} onChange={(event) => update(event.target.checked)} />
          : setting.kind === 'select'
            ? <select value={String(value)} onChange={(event) => update(event.target.value)}><option value="">{t('pl.select')}</option>{setting.options.map((option) => <option key={option} value={option}>{option}</option>)}</select>
            : setting.kind === 'textarea'
              ? <textarea rows={3} maxLength={setting.maxLength ?? undefined} value={value} onChange={(event) => update(event.target.value)} />
              : <input type={setting.kind === 'integer' ? 'number' : 'text'} min={setting.min ?? undefined} max={setting.max ?? undefined} value={value} onChange={(event) => update(setting.kind === 'integer' ? Number(event.target.value) : event.target.value)} />}</label>;
      })}</div>}
      <button className="btn primary" disabled={busy !== null || details.settings.definitions.length === 0} onClick={saveSettings}>{busy === `settings:${details.id}` ? '…' : t('pl.saveSettings')}</button>
      {details.manifest.actions.length > 0 && <><h3>{t('pl.actions')}</h3><div className="btn-row plugin-action-row">{details.manifest.actions.map((action) => <button className="btn" key={action.id} disabled={busy !== null} onClick={() => invoke(details.id, action.id)}>{action.title}</button>)}</div></>}
    </section>}
  </>;
}
