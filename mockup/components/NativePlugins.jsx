'use client';

import { useEffect, useState } from 'react';
import PageHead from '@/components/PageHead';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';
import { fetchPluginLogs, fetchPluginManifest, fetchPluginQualifications, fetchPluginSettings, fetchPluginTools, fetchPlugins, installPlugin, invokePluginAction, linkPlugin, previewPlugin, setPluginEnabled, uninstallPlugin, updatePluginSettings } from '@/lib/native-api';

export default function NativePlugins() {
  const t = useT();
  const data = useData();
  const [plugins, setPlugins] = useState([]);
  const [path, setPath] = useState('');
  const [source, setSource] = useState({ owner: '', repo: '', subdir: '', commit: '' });
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
    try { await fn(); setDetails(null); await load(); }
    catch (cause) { setError(cause.message); }
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
    if (!path.trim()) return;
    await operation('link', () => linkPlugin(path.trim()));
    setPath('');
  }
  async function previewSource() {
    setBusy('preview'); setError(null);
    try {
      const commit = source.commit.trim();
      if (!/^[a-f0-9]{40}$/.test(commit) || !source.owner.trim() || !source.repo.trim()) throw new Error('owner, repo, and a lowercase 40-hex commit are required');
      setPreview(await previewPlugin({ kind: 'github', owner: source.owner.trim(), repo: source.repo.trim(), subdir: source.subdir.trim() || null, commit }));
    } catch (cause) { setError(cause.message); }
    finally { setBusy(null); }
  }
  async function install() {
    if (!preview) return;
    await operation('install', () => installPlugin(preview.previewId, preview.sourceSha256));
    setPreview(null);
  }

  return <>
    <PageHead title={t('pl.title')} sub={t('pl.sub')} />
    <section className="panel" aria-labelledby="plugin-link">
      <h2 id="plugin-link" className="sec">{t('pl.link')}</h2>
      <div className="btn-row">
        <label className="grow">{t('pl.path')} <input value={path} onChange={(event) => setPath(event.target.value)} /></label>
        <button className="btn primary" disabled={!path.trim() || busy !== null} onClick={link}>{busy === 'link' ? '…' : t('pl.link')}</button>
      </div>
      <p className="dim">{t('pl.previewHelp')}</p>
      <div className="btn-row">
        {['owner', 'repo', 'subdir', 'commit'].map((field) => <label key={field} className="grow">{field}<input value={source[field]} onChange={(event) => setSource((old) => ({ ...old, [field]: event.target.value }))} /></label>)}
      </div>
      <button className="btn" disabled={!source.owner.trim() || !source.repo.trim() || busy !== null} onClick={previewSource}>{busy === 'preview' ? '…' : t('pl.preview')}</button>
      {preview && <div className="notice" role="status">
        <p><b>{preview.manifest?.name ?? preview.manifest?.id}</b></p>
        <p className="mono-s">{t('pl.digest')}: {preview.sourceSha256}</p>
        <button className="btn primary" disabled={busy !== null} onClick={install}>{busy === 'install' ? '…' : t('pl.install')}</button>
      </div>}
    </section>
    <section aria-labelledby="plugin-list">
      <h2 id="plugin-list" className="sec">{t('pl.title')}</h2>
      {phase === 'loading' && <p role="status">{t('nr.loading')}</p>}
      {error && <p className="notice bad" role="alert">{t('pl.failed', { error })}</p>}
      {phase === 'ready' && plugins.length === 0 && <p className="empty">{t('pl.empty')}</p>}
      {plugins.length > 0 && <div className="tbl-wrap"><table className="tbl"><thead><tr><th>ID</th><th>Version</th><th>Status</th><th>Revision / leases</th><th>{t('pl.digest')}</th><th /></tr></thead><tbody>{plugins.map((plugin) => <tr key={plugin.id}>
        <td className="mono-s">{plugin.id}</td><td>{plugin.manifestVersion}</td><td>{plugin.readiness}{plugin.stale ? ' · stale' : ''}{plugin.restartRequired ? ' · restart required' : ''}</td><td>{Object.keys(plugin.revisions ?? {}).length} / {plugin.activeLeases ?? 0}</td><td className="mono-s">{plugin.activePackageSha256 ?? plugin.packageSha256}</td>
        <td className="btn-row">
          <button className="btn" disabled={busy !== null} onClick={() => inspectPlugin(plugin.id)}>{busy === `inspect:${plugin.id}` ? '…' : 'Inspect'}</button>
          {plugin.enabled
            ? <button className="btn" disabled={busy !== null} onClick={() => operation(plugin.id, () => setPluginEnabled(plugin.id, false))}>{t('pl.disable')}</button>
            : <button className="btn primary" disabled={busy !== null} onClick={() => operation(plugin.id, () => setPluginEnabled(plugin.id, true))}>{t('pl.enable')}</button>}
          <button className="btn danger" disabled={busy !== null || plugin.enabled} onClick={() => operation(plugin.id, () => uninstallPlugin(plugin.id))}>{t('pl.uninstall')}</button>
        </td>
      </tr>)}</tbody></table></div>}
    </section>
    {details && <section className="panel" aria-label="Plugin execution details">
      <div className="btn-row"><h2 className="sec grow" style={{ marginTop: 0 }}>{details.manifest.name} · {details.id}</h2><button className="btn" disabled={busy !== null} onClick={refreshLogs}>{busy === `logs:${details.id}` ? '…' : 'Refresh activity'}</button></div>
      <p className="mono-s">package {plugins.find((plugin) => plugin.id === details.id)?.packageSha256}</p>
      <h3>Approval-gated tools</h3>
      {details.tools.length === 0 ? <p className="dim">None</p> : <div className="tbl-wrap"><table className="tbl"><thead><tr><th>Tool</th><th>Description</th><th>Input schema</th><th>Package</th></tr></thead><tbody>{details.tools.map((tool) => <tr key={tool.toolId}><td><code>{tool.name}</code></td><td>{tool.description}</td><td><pre className="cmd">{JSON.stringify(tool.inputSchema)}</pre></td><td className="mono-s">{tool.packageSha256}</td></tr>)}</tbody></table></div>}
      <h3>Runtime adapters</h3>
      {details.manifest.runtimes.length === 0 ? <p className="dim">None</p> : <ul>{details.manifest.runtimes.map((runtime) => <li key={runtime.id}><code>{runtime.id}</code> · {runtime.framework} · {runtime.protocol}</li>)}</ul>}
      <h3>Harness adapters</h3>
      {details.manifest.harnesses.length === 0 ? <p className="dim">None</p> : <ul>{details.manifest.harnesses.map((harness) => <li key={harness.id}><code>{harness.id}</code> · {harness.protocol}</li>)}</ul>}
      <h3>Qualified runtime choices</h3>
      {details.qualifications.length === 0 ? <p className="dim">No qualified runtime choice. Qualify this exact package before selecting it for a resource.</p> : <div className="tbl-wrap"><table className="tbl"><thead><tr><th>Framework</th><th>Model</th><th>Provider</th><th>Reasoning</th><th>Roles</th></tr></thead><tbody>{details.qualifications.map((choice) => <tr key={`${choice.adapterId}-${choice.framework}-${choice.model}-${choice.reasoning ?? ''}`}><td>{choice.framework}</td><td className="mono-s">{choice.model}</td><td>{choice.provider}</td><td>{choice.reasoning ?? 'default'}</td><td>{choice.roles.join(', ')}</td></tr>)}</tbody></table></div>}
      <h3>Execution activity</h3>
      {details.logs.length === 0 ? <p className="dim">No plugin process invocation has been recorded.</p> : <pre className="cmd">{details.logs.map((entry) => `[${new Date(entry.atMs).toISOString()}] ${entry.stream}\n${entry.text}`).join('\n')}</pre>}
      <h3>Plugin settings</h3>
      <p className="dim">Revision {details.settings.document.revision} · {details.settings.settingsReady ? 'ready' : `missing: ${details.settings.missingRequired.join(', ')}`}</p>
      {details.settings.definitions.length === 0 ? <p className="dim">This plugin has no settings.</p> : <div className="form-grid">{details.settings.definitions.map((setting) => {
        const value = Object.hasOwn(draftSettings, setting.id) ? draftSettings[setting.id] : (details.settings.document.values[setting.id] ?? setting.default ?? (setting.kind === 'boolean' ? false : ''));
        const update = (next) => setDraftSettings((old) => ({ ...old, [setting.id]: next }));
        return <label key={setting.id} className="grow">{setting.label}{setting.required ? ' *' : ''}<small className="dim">{setting.description}</small>{setting.kind === 'boolean'
          ? <input type="checkbox" checked={value === true} onChange={(event) => update(event.target.checked)} />
          : setting.kind === 'select'
            ? <select value={String(value)} onChange={(event) => update(event.target.value)}><option value="">Select…</option>{setting.options.map((option) => <option key={option} value={option}>{option}</option>)}</select>
            : setting.kind === 'textarea'
              ? <textarea rows={3} maxLength={setting.maxLength ?? undefined} value={value} onChange={(event) => update(event.target.value)} />
              : <input type={setting.kind === 'integer' ? 'number' : 'text'} min={setting.min ?? undefined} max={setting.max ?? undefined} value={value} onChange={(event) => update(setting.kind === 'integer' ? Number(event.target.value) : event.target.value)} />}</label>;
      })}</div>}
      <button className="btn primary" disabled={busy !== null || details.settings.definitions.length === 0} onClick={saveSettings}>{busy === `settings:${details.id}` ? '…' : 'Save settings'}</button>
      {details.manifest.actions.length > 0 && <><h3>Actions</h3><div className="btn-row">{details.manifest.actions.map((action) => <button className="btn" key={action.id} disabled={busy !== null} onClick={() => invoke(details.id, action.id)}>{action.title}</button>)}</div></>}
    </section>}
  </>;
}
