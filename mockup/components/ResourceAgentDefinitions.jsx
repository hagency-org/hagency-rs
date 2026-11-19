'use client';

import { useEffect, useState } from 'react';
import { useT } from '@/components/Prefs';
import { send } from '@/lib/api';
import { fetchPluginManifest, fetchPlugins } from '@/lib/native-api';
function valueFor(setting, values) {
  return Object.hasOwn(values, setting.id) ? values[setting.id] : setting.default ?? '';
}

function SettingControl({ setting, value, onChange }) {
  const common = { id: `harness-setting-${setting.id}`, value: value ?? '', onChange: (event) => onChange(setting.kind === 'integer' ? Number(event.target.value) : event.target.value) };
  if (setting.kind === 'boolean') return <input {...common} type="checkbox" checked={value === true} onChange={(event) => onChange(event.target.checked)} />;
  if (setting.kind === 'select') return <select {...common}>{(setting.options ?? []).map((option) => <option key={option} value={option}>{option}</option>)}</select>;
  if (setting.kind === 'integer') return <input {...common} type="number" min={setting.min ?? undefined} max={setting.max ?? undefined} />;
  if (setting.kind === 'textarea') return <textarea {...common} rows={3} maxLength={setting.maxLength ?? undefined} />;
  return <input {...common} type="text" />;
}

export default function ResourceAgentDefinitions({ preset, roles, live, refresh }) {
  const t = useT();
  const [definitions, setDefinitions] = useState(preset.agentDefinitions ?? []);
  const [name, setName] = useState('');
  const [role, setRole] = useState('');
  const [harnessId, setHarnessId] = useState(preset.harnessPlugin?.harnessId ?? '');
  const [templateValues, setTemplateValues] = useState({});
  const [manifest, setManifest] = useState(null);
  const [busy, setBusy] = useState(null);
  const [error, setError] = useState(null);

  const identity = preset.harnessPlugin;
  const harnesses = manifest?.harnesses ?? [];
  const selectedHarness = harnesses.find((harness) => harness.id === harnessId) ?? harnesses[0] ?? null;
  const settings = selectedHarness?.configuration ?? [];
  const requiredReady = settings.every((setting) => !setting.required || (templateValues[setting.id] ?? valueFor(setting, templateValues)) !== '');

  async function loadDefinitions() {
    const result = await send(`framework-presets/${encodeURIComponent(preset.id)}/agents`);
    if (!result.ok) throw new Error(result.error ?? 'agent_definitions_unavailable');
    const rows = result.body?.agentDefinitions;
    if (!Array.isArray(rows)) throw new Error('invalid_agent_definitions');
    if (identity && rows.some((definition) => {
      const template = definition.harnessTemplate;
      return template && (template.pluginId !== identity.pluginId
        || template.harnessId !== identity.harnessId
        || template.packageSha256 !== identity.packageSha256
        || template.settingsSha256 !== identity.settingsSha256);
    })) throw new Error('harness_template_identity_mismatch');
    setDefinitions(rows);
  }
  useEffect(() => {
    setDefinitions(preset.agentDefinitions ?? []);
    setHarnessId(identity?.harnessId ?? '');
    setTemplateValues({});
    setError(null);
    if (!identity?.pluginId) { setManifest(null); return undefined; }
    let active = true;
    Promise.all([fetchPlugins(), fetchPluginManifest(identity.pluginId)]).then(async ([plugins, value]) => {
      const record = plugins.find((candidate) => candidate.id === identity.pluginId);
      const packageSha256 = record?.activePackageSha256 ?? record?.packageSha256;
      if (!record || !/^[a-f0-9]{64}$/.test(packageSha256 ?? '') || packageSha256 !== identity.packageSha256 || record.settingsSha256 !== identity.settingsSha256) throw new Error('plugin_identity_stale');
      if (active) setManifest(value);
      await loadDefinitions();
    }).catch((cause) => { if (active) setError(cause.message); });
    return () => { active = false; };
  }, [preset.id, identity?.pluginId, identity?.harnessId, identity?.packageSha256, identity?.settingsSha256]);
  useEffect(() => {
    if (!harnessId && harnesses[0]) setHarnessId(harnesses[0].id);
  }, [harnessId, harnesses]);

  async function mutate(path, method, body) {
    setBusy(path); setError(null);
    try {
      const result = await send(path, { method, body });
      if (!result.ok) { setError(result.error); return false; }
      await refresh();
      await loadDefinitions();
      return true;
    } catch (cause) { setError(cause.message); return false; }
    finally { setBusy(null); }
  }
  const templateBody = identity ? { harnessId: selectedHarness?.id ?? harnessId, harnessTemplateValues: templateValues } : {};
  return <div data-testid={`resource-definitions-${preset.id}`}>
    <p className="note">{t('rad.help')}</p>
    {definitions.length > 0 && <table className="tbl" style={{ marginTop: 8 }}>
      <thead><tr><th>{t('col.agent')}</th><th>{t('col.role')}</th><th>{t('rad.harness')}</th><th>{t('rad.enabled')}</th><th>{t('rad.status')}</th><th>{t('rad.active')}</th><th /></tr></thead>
      <tbody>{definitions.map((definition) => <tr key={definition.id}>
        <td className="mono-s">{definition.name}</td><td className="dim">{definition.role}</td>
        <td className="mono-s">{definition.harnessTemplate?.harnessId ?? identity?.harnessId ?? '—'}{definition.harnessTemplate && <small className="dim"> · {Object.keys(definition.harnessTemplate.values ?? {}).length} values</small>}</td>
        <td><button className="btn" disabled={!live || busy === definition.id} onClick={() => mutate(`framework-presets/${encodeURIComponent(preset.id)}/agents/${encodeURIComponent(definition.id)}`, 'PUT', { enabled: !definition.enabled })}>{t(definition.enabled ? 'rad.disable' : 'rad.enable')}</button></td>
        <td><span className={`badge ${definition.status === 'provisioned' ? 'ok' : definition.status === 'reserved' ? 'warn-b' : ''}`}>{t(`rad.${definition.status}`)}</span></td>
        <td>{definition.activeEngagements}</td>
        <td><button className="btn" disabled={!live || busy === definition.id} onClick={() => mutate(`framework-presets/${encodeURIComponent(preset.id)}/agents/${encodeURIComponent(definition.id)}`, 'DELETE')}>{t('act.delete')}</button></td>
      </tr>)}</tbody>
    </table>}
    {definitions.length === 0 && <p className="dim">{t('rad.none')}</p>}
    {identity && <section className="panel" style={{ marginTop: 8 }}>
      <h4>{t('rad.harnessTitle')}</h4>
      {!manifest && <p role="status">{t('rad.harnessLoading')}</p>}
      {manifest && <>
        <label>{t('rad.harness')}<select value={selectedHarness?.id ?? ''} disabled={!live} onChange={(event) => { setHarnessId(event.target.value); setTemplateValues({}); }}>
          {harnesses.map((harness) => <option key={harness.id} value={harness.id}>{harness.id}</option>)}
        </select></label>
        {settings.map((setting) => <label key={setting.id}>{setting.label}<SettingControl setting={setting} value={valueFor(setting, templateValues)} onChange={(value) => setTemplateValues((old) => ({ ...old, [setting.id]: value }))} /><small className="dim">{setting.description}{setting.maxLength ? ` · max ${setting.maxLength}` : ''}</small></label>)}
        {!requiredReady && <p className="warn-text">{t('rad.harnessRequired')}</p>}
      </>}
    </section>}
    <div className="btn-row" style={{ marginTop: 8 }}>
      <input placeholder={t('rad.name')} value={name} disabled={!live} onChange={(event) => setName(event.target.value)} style={{ width: 180 }} />
      <select value={role} disabled={!live} onChange={(event) => setRole(event.target.value)}><option value="">{t('rad.pickRole')}</option>{(roles ?? []).map((value) => <option key={value} value={value}>{value}</option>)}</select>
      <button className="btn" disabled={!live || busy === 'create' || !name || !role || (identity && (!manifest || !selectedHarness || !requiredReady))} onClick={async () => {
        const okDone = await mutate(`framework-presets/${encodeURIComponent(preset.id)}/agents`, 'POST', { name, role, ...templateBody });
        if (okDone) { setName(''); setRole(''); setTemplateValues({}); }
      }}>{t('rad.add')}</button>
    </div>
    {error && <p role="alert" className="warn-text">{error}</p>}
  </div>;
}
