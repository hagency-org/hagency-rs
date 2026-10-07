'use client';
import PageHead from '@/components/PageHead';
import NativeStatusStrip from '@/components/NativeStatusStrip';
import TechnicalDetails from '@/components/TechnicalDetails';
import ResourceAgents from '@/components/ResourceAgents';
import { NativeAccessNotice } from '@/components/NativeUsage';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';
import { errorText } from '@/lib/i18n';
import { Blank } from '@/components/Blank';
import SearchSelect from '@/components/SearchSelect';
import ResourceCapacity from '@/components/ResourceCapacity';
import { labelFor } from '@/lib/labels';

const label = (r) => [r.framework, r.model, r.reasoning].filter(Boolean).join(' · ');
function BudgetPart({ value, account = false }) {
  const t = useT(); const number = (n) => n == null ? t('nu.unknown') : n.toLocaleString();
  return <section className="panel" style={{ marginTop: 0 }}><h3>{t(account ? 'nr.account' : 'nr.pool')}</h3><dl className="kv">
    <dt>{t(account ? 'nr.quota' : 'col.ceiling')}</dt><dd data-budget={account ? 'quota' : 'ceiling'}>{number(account ? value.quota : value.ceiling)}</dd>
    <dt>{t('nr.period')}</dt><dd>{value.period === 'monthly' ? t('rs.monthly') : value.period ?? t('nu.unknown')}</dd>
    <dt>{t('nr.committed')}</dt><dd>{number(value.committed)}</dd>
    <dt>{t('nr.remaining')}</dt><dd>{number(value.remaining)}</dd>
    {account && <><dt>{t('nr.declaration')}</dt><dd>{t(`nr.seat.${value.status}`)}</dd></>}
  </dl></section>;
}
function HeadroomPart({ draw, number, t }) {
  // Unknown is unknown, never zero: every null below renders the explicit
  // unknown word, so an unmeasured period never reads as a free resource.
  // Brief 20 (E3): no separate "committed" cell here — the budget panel's
  // "Committed allocation" already shows pool.committed, which equals
  // draw.committed by the resource_id = public_resource_id(preset_id)
  // invariant (pinned by the shared-seat console test; ADR-121).
  const binding = draw.binding === null
    ? t('nr.headroom.noCompetition')
    : draw.binding === 'measured spend' ? t('nr.headroom.bindingMeasured') : t('nr.headroom.bindingCommitted');
  return <section className="panel" data-headroom>
    <h3>{t('nr.headroom.title')}</h3>
    <dl className="kv">
      <dt>{t('nr.headroom.drawn')}</dt><dd data-headroom="drawn">{number(draw.drawn)}</dd>
      <dt>{t('nr.headroom.measured')}</dt><dd data-headroom="measured">{number(draw.measured)}</dd>
      <dt>{t('nr.headroom.consumed')}</dt><dd data-headroom="consumed">{number(draw.consumed)}</dd>
      <dt>{t('nr.headroom.binding')}</dt><dd data-headroom="binding">{binding}</dd>
      <dt>{t('nr.headroom.ceiling')}</dt><dd data-headroom="ceilingTokens">{number(draw.ceilingTokens)}</dd>
      <dt>{t('nr.headroom.remaining')}</dt><dd data-headroom="remainingBeforeCeiling">{number(draw.remainingBeforeCeiling)}</dd>
    </dl>
    <p className="note">{t('nr.headroom.meaning')}</p>
  </section>;
}
export default function NativeResources() {
  const t = useT(); const data = useData();
  const { phase, selected, resources = [], roles = [], budget, action } = data;
  const configured = data.coordinatorResources ? resources.filter(r => r.engagementResources.length) : resources;
  const number = (n) => n == null ? t('nu.unknown') : n.toLocaleString();
  return <>
    <PageHead title={t('rs.title')} sub={t('nr.sub')}>
      {/* The page's one primary action lives in its header. */}
      {['ready', 'stale'].includes(phase) && (resources.length > 0 || selected) && <a className="btn primary" href={`/console/resources/new/${selected ? `?source_resource_id=${selected}` : ''}`}>{t('nr.createLink')}</a>}
      <NativeStatusStrip />
    </PageHead>
    <p className="muted notice">{t(data.coordinatorResources ? 'nc.engagementHelp' : 'nr.localOnly')}</p>
    {/* Creating a resource needs a SOURCE resource to derive from, so with none
        configured this link landed on `nc.noSource` — a dead end two clicks in.
        The empty state below names the real first step (configure the local coding tool in Setup), so
        the create control appears only when it can actually be completed
        (#44 item 11).
        Item 3 (lane nav): the list link names the destination; the wizard's own
        submit keeps "Create resource" as the action. Both hold. */}

    {phase === 'loading' && <p role="status">{t('nr.loading')}</p>}
    <NativeAccessNotice />
    {action && <section className="notice" data-resource-action={action.kind} role={action.kind === 'pending' || action.kind === 'saved' ? 'status' : 'alert'}>
      <p><b>{action.label}</b> · {t(`nr.action.${action.kind}`)}</p>
      {['conflict', 'unknown', 'busy', 'refused'].includes(action.kind) && <button className="btn" disabled={phase === 'access'} onClick={data.refresh}>{t('nr.reconcile')}</button>}
    </section>}
    {phase === 'error' && <section className="panel" role="alert"><h2>{t('nr.failed')}</h2><p>{t(data.error === 'not_found' ? 'nr.notFound' : 'nu.retryHelp')}</p><button className="btn" onClick={data.refresh}>{t('nu.refresh')}</button></section>}
    {['ready', 'stale'].includes(phase) && <div data-native-resource-state={phase} aria-busy={data.refreshing === true}>
      {data.refreshing && <p role="status">{t('nr.refreshing')}</p>}
      {phase === 'stale' && <p role="alert">{t('nr.stale')}</p>}
      <section className="panel"><h2 className="sec" style={{ marginTop: 0 }}>{t('nr.profile')}</h2>
        <div className="field"><label htmlFor="native-resource">{t('nr.selected')}</label><SearchSelect
          id="native-resource"
          placeholder={t('nc.sourceSearch')}
          value={selected}
          onChange={(event) => data.choose(event.target.value)}
          options={configured.map((r) => ({ value: r.id, label: label(r) }))}
          outside={labelFor(selected) ?? t('nr.outsidePage')}
          empty={<>{t(resources.length ? 'nc.unassigned' : 'nr.empty')} {!resources.length && <a href="/console/setup/">{t('nr.emptySetup')}</a>}</>}
        /></div>
        {!data.permissions?.publishResource && <div className="notice"><p>{t(data.permissions?.configureResource ? 'nc.publicationSeparate' : 'nr.readOnly')}</p><code>hagency console-access --state-dir &lt;state&gt; --listen &lt;address&gt;</code></div>}
        <div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('nr.profile')}</th><th>{t(data.coordinatorResources ? 'rc.remainingTotal' : 'col.ceiling')}</th><th>{t('nr.catalog')}</th><th>{t('col.action')}</th></tr></thead><tbody>
          {configured.map((r) => <tr key={r.id} data-resource-row={r.id}><td>{label(r)}{r.provider && <div className="dim">{r.provider}</div>}<TechnicalDetails><code>{r.id}</code></TechnicalDetails></td><td>{!data.coordinatorResources ? number(r.ceiling?.tokens) : r.engagementResources.map(g => <div key={g.id}><ResourceCapacity total={g.allocatedTokens} remaining={data.resourceCapacities?.find(c => c.id === g.id && c.revision === g.revision)?.remainingTokens} retained={data.resourceCapacities?.find(c => c.id === g.id && c.revision === g.revision)?.retainedTokens} stale={phase === 'stale'} /><div className="dim">{g.serverEngagementId}</div></div>)}</td><td data-publication={r.published}>{t(r.published ? 'nr.included' : 'nr.withdrawn')}</td><td><a className="btn" href={`/console/resources/new/?resource_id=${r.id}`}>{t('nc.edit')}</a> <ResourceAgents preset={r} native={{ allowed: data.permissions?.publishResource && phase === 'ready', busy: action?.kind === 'pending', publish: data.publish }} /></td></tr>)}
        </tbody></table></div>
        {/* Table footer: what the page covers, then the pager. */}
        <div className="table-foot"><p className="dim">{t('nr.pageOnly')}</p>
          <div className="btn-row"><button className="btn" onClick={data.refresh}>{t('nu.refresh')}</button>{(data.current_after || data.next_after) && <><button className="btn" disabled={!data.current_after} onClick={data.firstPage}>{t('nu.firstPage')}</button><button className="btn" disabled={!data.next_after} onClick={data.nextPage}>{t('nu.nextPage')}</button></>}</div>
        </div>
      </section>
      {budget && (data.coordinatorResources ? <section className="panel" data-resource-id={selected}>
        <h2 className="sec" style={{ marginTop: 0 }}>{t('nr.budget')}</h2>
        {(data.engagementResources ?? []).map(g => <section key={g.id} data-engagement-resource={g.id}>
          <p>{t('nc.engagement')}: {g.serverEngagementId}</p>
          <ResourceCapacity total={g.allocatedTokens} remaining={g.remainingTokens} retained={g.retainedTokens} stale={phase === 'stale'} />
          <p className="dim">{t('rc.meaning')}</p>
          <dl className="kv"><dt>{t('col.ceiling')}</dt><dd data-budget="ceiling">{number(g.allocatedTokens)}</dd>
            <dt>{t('se.retained')}</dt><dd>{number(g.retainedTokens)}</dd>
            <dt>{t('se.available')}</dt><dd data-budget="remaining">{number(g.remainingTokens)}</dd>
            <dt>{t('se.managers')}</dt><dd>{g.eligibleManagers.join(', ')}</dd>
          </dl>
        </section>)}
        {!data.engagementResources?.length && <p>{t('nc.unassigned')}</p>}
        <TechnicalDetails><BudgetPart value={budget.seat} account /><p>{t('nr.budgetMeaning')}</p>{budget.draw && <HeadroomPart draw={budget.draw} number={number} t={t} />}</TechnicalDetails>
      </section> : <section className="panel" data-resource-id={selected}><h2 className="sec" style={{ marginTop: 0 }}>{t('nr.budget')}</h2><p>{t('nr.budgetMeaning')}</p><div className="split even"><BudgetPart value={budget.pool} /><BudgetPart value={budget.seat} account /></div><p>{t('nr.effectiveRemaining')}: <b>{number(budget.remainingTokens)}</b></p>{budget.draw && <HeadroomPart draw={budget.draw} number={number} t={t} />}</section>)}
      <section className="panel"><h2 className="sec" style={{ marginTop: 0 }}>{t('nr.roles')}</h2><div className="tbl-wrap"><table className="tbl"><thead><tr><th>{t('nr.role')}</th><th>{t('nr.choice')}</th><th>{t('nr.eligible')}</th><th>{t('nr.fillable')}</th><th>{t('nr.families')}</th><th>{t('nr.overTier')}</th><th>{t('nr.crossFamily')}</th></tr></thead><tbody>{roles.map((r) => <tr key={r.role} data-role-row={r.role}><td>{r.role}</td><td>{t(r.explicitPublication === null ? 'nr.automatic' : r.explicitPublication ? 'nr.enabled' : 'nr.disabled')}</td><td>{t(r.available ? 'nr.yes' : 'nr.no')}</td><td data-fillable={r.fillable}>{r.fillable}</td><td data-families={r.families.join(' ')}>{r.families.length ? r.families.join(', ') : <Blank why="rs.why.noTier" t={t} />}</td><td data-over-tier={r.overTier}>{r.overTier}</td><td>{t(r.crossFamily ? 'nr.yes' : 'nr.no')}</td></tr>)}</tbody></table></div></section>
    </div>}
    <TechnicalDetails><p>{t('nr.roleMeaning')}</p><p>{t('nr.gaps')}</p>{selected && <p>{t('nr.identifier')}: <code>{selected}</code></p>}{data.error && <code>{errorText(t, data.error)}</code>}{action?.error && <code>{errorText(t, action.error)}</code>}</TechnicalDetails>
  </>;
}
