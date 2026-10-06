'use client';
import { useState } from 'react';
import { fetchAgentSettlement, settleAgentUsage } from '@/lib/native-api';
import { useT } from '@/components/Prefs';

export default function NativeSettlement({ agent, onSaved }) {
  const t = useT();
  const [value, setValue] = useState(null);
  const [tokens, setTokens] = useState('');
  const [reference, setReference] = useState('');
  const [command, setCommand] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function load() {
    if (busy) return;
    setBusy(true); setError('');
    try { setValue(await fetchAgentSettlement(agent.id)); }
    catch (e) { setError(e.message); }
    finally { setBusy(false); }
  }
  async function save() {
    if (busy || !value?.canSettle) return;
    if (!/^(0|[1-9][0-9]*)$/.test(tokens) || !Number.isSafeInteger(Number(tokens)) || !reference.trim()) { setError(t('se.finalInvalid')); return; }
    // Keep the exact intent after an uncertain reply, including across edits.
    const body = command ?? { commandId: `settle_${crypto.randomUUID().replaceAll('-', '')}`, agentAllocationId: agent.id,
      resourceAllocationId: value.resourceAllocationId, expectedAllocatedTokens: value.allocatedTokens,
      consumedTokens: Number(tokens), period: value.period, periodKey: value.periodKey, evidenceReference: reference.trim() };
    setCommand(body); setBusy(true); setError('');
    try { setValue(await settleAgentUsage(body)); await onSaved(); }
    catch (e) {
      // These refusals prove that no settlement committed. Network failures
      // retain the frozen intent so retry can only replay the same command.
      if (['invalid_engagement_id', 'engagement_not_live', 'not_found', 'command_conflict'].includes(e.message)) setCommand(null);
      setError(e.message);
    }
    finally { setBusy(false); }
  }
  return <details data-agent-settlement={agent.id} onToggle={(e) => { if (e.currentTarget.open && !value) load(); }}>
    <summary>{t('se.finalUsage')}</summary>
    {error && <p role="alert">{error}</p>}
    {!value && <button className="btn-s" disabled={busy} onClick={load}>{t('nu.refresh')}</button>}
    {value && <>
      <p>{value.period} · {value.periodKey}</p>
      {value.state === 'awaiting_final_usage' ? <>
        <p>{t('se.finalExplanation')}</p>
        {value.canSettle ? <div className="stack">
          <label>{t('se.finalTokens')}<input aria-label={t('se.finalTokens')} inputMode="numeric" value={tokens} disabled={busy || !!command} onChange={(e) => setTokens(e.target.value)} /></label>
          <label>{t('se.finalReference')}<input aria-label={t('se.finalReference')} maxLength={256} value={reference} disabled={busy || !!command} onChange={(e) => setReference(e.target.value)} /></label>
          <button className="btn-s" disabled={busy} onClick={save}>{t('se.finalConfirm')}</button>
        </div> : <p>{t('se.finalWait')}</p>}
      </> : <p data-settlement-result>{t('se.finalResult', { consumed: value.consumedTokens, released: value.releasedTokens, late: value.lateUsageTokens ?? 0 })}</p>}
    </>}
  </details>;
}
