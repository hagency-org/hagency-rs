'use client';
import { useState } from 'react';
import { engagementUpgradeAct, fetchEngagementUpgrade, fetchResourceRollout, recordCanaryResult, resourceRolloutAct } from '@/lib/native-api';
import { useT } from '@/components/Prefs';

const runtime = (r) => (r ? `${r.pluginId}/${r.adapterId} ${String(r.packageSha256).slice(0, 12)}` : '—');

/* ADR-197 operator surface: one engagement's runtime upgrade state, pin and
 * rollback, and its resource's canary rollout. Every refusal shows the store's
 * word (dispatch_started, unqualified, ...); nothing here retries or forces. */
export default function NativeUpgrade({ agent }) {
  const t = useT();
  const [upgrade, setUpgrade] = useState(null);
  const [rollout, setRollout] = useState(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState('');
  async function load() {
    setBusy(true); setNote('');
    try {
      const u = await fetchEngagementUpgrade(agent.id);
      setUpgrade(u);
      setRollout(await fetchResourceRollout(u.resourceId));
    } catch (e) { setNote(e.message); }
    finally { setBusy(false); }
  }
  async function run(kind, action) {
    if (busy) return;
    setBusy(true); setNote('');
    try {
      if (kind === 'engagement') await engagementUpgradeAct(agent.id, action);
      else if (action === 'canary-pass' || action === 'canary-fail') await recordCanaryResult(upgrade.resourceId, action === 'canary-pass');
      else await resourceRolloutAct(upgrade.resourceId, action);
      const u = await fetchEngagementUpgrade(agent.id);
      setUpgrade(u);
      setRollout(await fetchResourceRollout(u.resourceId));
      setNote(t('ug.done', { action }));
    } catch (e) { setNote(e.message === 'agent_lifecycle_scope_required' ? t('ng.scopeRequired') : `${t('ng.actionFailed')} (${e.message})`); }
    finally { setBusy(false); }
  }
  const pinned = upgrade?.state === 'pinned';
  return <details data-engagement-upgrade={agent.id} onToggle={(e) => { if (e.currentTarget.open && !upgrade) load(); }}>
    <summary>{t('ug.title')}</summary>
    {note && <p role="status">{note}</p>}
    {upgrade && <>
      <p className="dim">
        {t('ug.state')}: {upgrade.state ?? t('ug.none')}{upgrade.blocker ? ` (${upgrade.blocker})` : ''}
        {' · '}{t('ug.current')}: {runtime(upgrade.currentRuntime)}
        {' · '}{t('ug.target')}: {runtime(upgrade.targetRuntime)}
        {upgrade.previousRuntime && <>{' · '}{t('ug.previous')}: {runtime(upgrade.previousRuntime)}</>}
      </p>
      <span className="btn-row tight">
        <button type="button" className="btn-s" disabled={busy || pinned || !upgrade.state} onClick={() => run('engagement', 'rebind')}>{t('ug.rebind')}</button>
        <button type="button" className="btn-s" disabled={busy} onClick={() => run('engagement', pinned ? 'unpin' : 'pin')}>{pinned ? t('ug.unpin') : t('ug.pin')}</button>
        <button type="button" className="btn-s danger" disabled={busy || !upgrade.previousRuntime} onClick={() => run('engagement', 'rollback')}>{t('ug.rollback')}</button>
      </span>
      {rollout && <p className="dim" data-rollout-state={rollout.state}>
        {t('ug.rollout')}: {rollout.state}{rollout.canaryEngagementId ? ` · canary ${rollout.canaryEngagementId} ${rollout.canaryResult ?? '…'}` : ''}{rollout.pending.length ? ` · ${t('ug.pending', { n: rollout.pending.length })}` : ''}
      </p>}
      <span className="btn-row tight">
        <button type="button" className="btn-s" disabled={busy} onClick={() => run('resource', 'start')}>{t('ug.start')}</button>
        <button type="button" className="btn-s" disabled={busy || rollout?.state !== 'running'} onClick={() => run('resource', 'step')}>{t('ug.step')}</button>
        <button type="button" className="btn-s" disabled={busy || rollout?.state !== 'running' || !rollout?.canaryEngagementId || rollout?.canaryResult} onClick={() => run('resource', 'canary-pass')}>{t('ug.canaryPass')}</button>
        <button type="button" className="btn-s danger" disabled={busy || rollout?.state !== 'running' || !rollout?.canaryEngagementId || rollout?.canaryResult} onClick={() => run('resource', 'canary-fail')}>{t('ug.canaryFail')}</button>
      </span>
    </>}
  </details>;
}
