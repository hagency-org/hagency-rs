'use client';

/*
 * The native agent detail (board #22, TS backend-v2.js:12155
 * GET /api/agents/:name): one agent the service knows, fetched from
 * /console/api/agents/<name> and rendered as identity, resource, rooms,
 * current dispatch and recent tasks. Every section renders exactly what
 * the route serves — null last_seen is unknown, never zero, and a null
 * dispatch renders as "no live dispatch", never invented. A 404 from the
 * route means the native service knows no agent by this name; that is
 * said plainly, with the way back to the roster.
 */
import { useEffect, useState } from 'react';
import Link from 'next/link';
import { useT } from '@/components/Prefs';
import { fmtTokens } from '@/lib/mock-data';
import { deleteAgent, fetchAgentDetail, fetchAgentDeliveryEvents, fetchAgentLaunchEnv, requestAgentAvatar, undeleteAgent } from '@/lib/native-api';
import { useData } from '@/components/Data';

/* The launch environment (board #49, TS backend-v2.js:12344
 * GET /api/agents/:name/launch-env). It is a SEPARATE read from the detail —
 * the TS route is its own endpoint — so a failure here never blanks the
 * detail: the panel says the profile could not be read, and the rest of the
 * page stays. A null profile is "no runtime profile recorded", never an
 * invented default. */
function LaunchEnv({ name }) {
  const t = useT();
  const [profile, setProfile] = useState(undefined);
  useEffect(() => {
    let live = true;
    fetchAgentLaunchEnv(name)
      .then((value) => { if (live) setProfile(value.runtimeProfile); })
      .catch(() => { if (live) setProfile('error'); });
    return () => { live = false; };
  }, [name]);
  const role = (label, value) => (
    <tr>
      <th>{label}</th>
      <td className="dim">
        {value === null ? '—' : <>{`${value.framework} · ${value.model}${value.reasoning ? ` · ${value.reasoning}` : ''}`}{value.runtimePlugin && <><br /><span className="mono-s">runtime: {value.runtimePlugin.pluginId}/{value.runtimePlugin.adapterId}</span></>}{value.harnessPlugin && <><br /><span className="mono-s">harness: {value.harnessPlugin.pluginId}/{value.harnessPlugin.harnessId}</span></>}</>}
      </td>
    </tr>
  );
  return (
    <section className="panel" aria-label={t('na.launchEnv')}>
      <h3>{t('na.launchEnv')}</h3>
      {profile === undefined ? (
        <p className="dim">{t('na.detailLoading')}</p>
      ) : profile === 'error' ? (
        <p className="dim">{t('na.detailFailed')}</p>
      ) : profile === null ? (
        <p className="dim">{t('na.launchEnvNone')}</p>
      ) : (
        <table>
          <tbody>
            {role(t('na.launchEnvPrimary'), profile.primary)}
            {role(t('na.launchEnvSupervisor'), profile.supervisor)}
          </tbody>
        </table>
      )}
    </section>
  );
}

/* The delivery events (board #49, TS backend-v2.js:16988
 * GET /api/agents/:name/delivery-events): a SEPARATE read from the detail,
 * same independence rule as LaunchEnv — a failure here never blanks the
 * page. The list renders exactly what the route serves, newest first: an
 * empty list is "none recorded", never an invented row. */
function DeliveryEvents({ name }) {
  const t = useT();
  const [events, setEvents] = useState(undefined);
  useEffect(() => {
    let live = true;
    fetchAgentDeliveryEvents(name)
      .then((value) => { if (live) setEvents(value.events); })
      .catch(() => { if (live) setEvents('error'); });
    return () => { live = false; };
  }, [name]);
  return (
    <section className="panel" aria-label={t('na.deliveryEvents')}>
      <h3>{t('na.deliveryEvents')}</h3>
      {events === undefined ? (
        <p className="dim">{t('na.deliveryEventsLoading')}</p>
      ) : events === 'error' ? (
        <p className="dim">{t('na.deliveryEventsFailed')}</p>
      ) : events.length === 0 ? (
        <p className="dim">{t('na.deliveryEventsNone')}</p>
      ) : (
        <table>
          <thead>
            <tr><th>{t('col.when')}</th><th>{t('na.deliveryEvents')}</th><th>{t('col.reason')}</th></tr>
          </thead>
          <tbody>
            {events.map((event) => (
              <tr key={event.id} data-delivery-event-id={event.id}>
                <td className="dim">{new Date(event.ts).toISOString()}</td>
                <td>{event.type}{event.messageId ? ` · ${event.messageId}` : ''}</td>
                <td className="dim">{event.reason ?? '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

/* The avatar request (board #49, TS backend-v2.js:16370). TS handed the
 * request to the bridge over SSE and answered a receipt immediately, so the
 * control shows the receipt it got — queued, refused, or unknown — never a
 * claim that the avatar changed. Native keeps the request durable (the
 * `avatar_requests` row is the SSE hand-off made restart-safe); what no
 * native component does yet is set `m.room.avatar`, so the receipt is the
 * whole effect here and is labelled as such. */
function AvatarControl({ name }) {
  const t = useT();
  const [outcome, setOutcome] = useState(null);
  const [pending, setPending] = useState(false);
  const request = async (force) => {
    if (pending) return;
    setPending(true);
    try {
      const value = await requestAgentAvatar(name, { generate: force, force });
      setOutcome(value.queued === true ? 'queued' : 'refused');
    } catch (error) {
      setOutcome(error?.message === 'busy' ? 'busy' : 'unknown');
    } finally {
      setPending(false);
    }
  };
  return (
    <section className="panel" aria-label={t('na.avatar')}>
      <h3>{t('na.avatar')}</h3>
      <div className="btn-row">
        <button className="btn" disabled={pending} onClick={() => void request(false)}>{t('na.avatarRequest')}</button>
        <button className="btn" disabled={pending} onClick={() => void request(true)}>{t('na.avatarRegenerate')}</button>
      </div>
      {outcome && (
        <p className={outcome === 'queued' ? 'dim' : 'notice'} data-avatar-outcome={outcome} role={outcome === 'queued' ? 'status' : 'alert'}>
          {t(`na.avatar${outcome === 'queued' ? 'Queued' : outcome === 'refused' ? 'Refused' : outcome === 'busy' ? 'Busy' : 'Unknown'}`)}
        </p>
      )}
    </section>
  );
}

/* Restore a deleted agent (board #49, TS backend-v2.js:12308). The
 * interesting outcome is the 404: `no tombstone found` means nothing was
 * deleted, and that is reported as plainly as the success, never as an
 * error to hide. The route answers `{ok, undeleted, name}` and the client
 * validator pins `undeleted`, so a bare `ok` can never report a restore. */
function UndeleteControl({ name }) {
  const t = useT();
  const [outcome, setOutcome] = useState(null);
  const [pending, setPending] = useState(false);
  const restore = async () => {
    if (pending) return;
    setPending(true);
    try {
      const value = await undeleteAgent(name);
      setOutcome(value.undeleted === true ? 'queued' : 'refused');
    } catch (error) {
      setOutcome(error?.message === 'not_found' ? 'none' : 'unknown');
    } finally {
      setPending(false);
    }
  };
  return (
    <section className="panel" aria-label={t('na.undelete')}>
      <h3>{t('na.undelete')}</h3>
      <div className="btn-row">
        <button className="btn" disabled={pending} onClick={() => void restore()}>{t('na.undelete')}</button>
      </div>
      {outcome && (
        <p className={outcome === 'queued' || outcome === 'none' ? 'dim' : 'notice'} data-undelete-outcome={outcome} role={outcome === 'queued' || outcome === 'none' ? 'status' : 'alert'}>
          {t(`na.undelete${outcome === 'queued' ? 'Queued' : outcome === 'none' ? 'None' : outcome === 'refused' ? 'Refused' : 'Unknown'}`)}
        </p>
      )}
    </section>
  );
}

export default function NativeAgentDetail({ name, onBack }) {
  const t = useT();
  const data = useData();
  const manageLifecycle = data?.permissions?.manageLifecycle === true;
  const [phase, setPhase] = useState('loading');
  const [detail, setDetail] = useState(null);
  /* Board #58: the retained console's own destructive idiom (`AgentActions.jsx`)
   * — the control sits in a `danger-zone` below everything else and asks for
   * the agent's NAME rather than a click, "because a confirm dialog is a reflex
   * whereas typing is a decision". Removal is `?force=true`, and the response
   * must actually say `deleted`, so a caller cannot report success off `ok`
   * alone (the retained route answers `ok:true, deprecated:true` for a SOFT
   * delete while the agent stays). */
  const [confirming, setConfirming] = useState(false);
  const [typed, setTyped] = useState('');
  const [removing, setRemoving] = useState(null);
  const [removed, setRemoved] = useState(false);
  /*
   * The roster renders this in place (the packaged console serves one static
   * HTML per route with no fallback, so /agents/<name> would 404 in
   * production); an in-place caller passes `onBack` and gets a control that
   * clears its selection. The routed dev page (AgentDetail) passes nothing
   * and keeps the link back to the roster.
   */
  const back = onBack ? (
    <button className="btn" onClick={onBack}>{t('na.detailBack')}</button>
  ) : (
    <Link className="btn" href="/agents">{t('na.detailBack')}</Link>
  );
  /* A removal cannot be reflected by an in-place read — the agent is gone —
   * so the page leaves for the roster, like the retained console does. */
  const remove = async () => {
    if (removing || typed !== name) return;
    setRemoving('pending');
    try {
      const value = await deleteAgent(name, true);
      if (value.deleted !== true) {
        setRemoving('refused');
        return;
      }
      setRemoving('saved');
      setRemoved(true);
      if (onBack) onBack();
    } catch (error) {
      setRemoving(['busy', 'outcome_unknown', 'native_unavailable', 'invalid_native_response'].includes(error?.message) ? 'unknown' : 'refused');
    }
  };

  useEffect(() => {
    let live = true;
    fetchAgentDetail(name)
      .then((value) => {
        if (!live) return;
        setDetail(value);
        setPhase('ready');
      })
      .catch((error) => {
        if (!live) return;
        setPhase(error?.message === 'not_found' ? 'missing' : 'error');
      });
    return () => { live = false; };
  }, [name]);

  if (phase === 'loading') {
    return (
      <section className="panel" role="status">
        <p>{t('na.detailLoading')}</p>
      </section>
    );
  }
  if (phase === 'missing') {
    return (
      <div className="empty">
        <div className="big">{t('na.detailNotFound', { name })}</div>
        {back}
      </div>
    );
  }
  if (phase === 'error') {
    return (
      <section className="panel" role="alert">
        <p>{t('na.detailFailed')}</p>
        {back}
      </section>
    );
  }

  return (
    <div data-agent-name={detail.name}>
      <h2 style={{ marginTop: 0 }}>{detail.name}<span className="note"> {t('na.readonly')}</span></h2>

      <section className="panel" aria-label={t('na.detailIdentity')}>
        <h3>{t('na.detailIdentity')}</h3>
        <table>
          <tbody>
            <tr><th>{t('col.framework')}</th><td>{detail.framework}</td></tr>
            <tr><th>{t('col.role')}</th><td>{detail.role}</td></tr>
            <tr><th>{t('col.state')}</th><td>{detail.state}</td></tr>
            <tr><th>{t('na.online')}</th><td>{detail.online ? t('na.online') : t('na.offline')}</td></tr>
            <tr><th>{t('na.lastSeen')}</th><td className="dim">{detail.last_seen_ms === null ? t('nu.unknown') : new Date(detail.last_seen_ms).toISOString()}</td></tr>
            <tr><th>{t('na.detailEngagements')}</th><td className="num">{detail.engagements}</td></tr>
            <tr><th>{t('na.detailProject')}</th><td className="dim">{detail.project_id}</td></tr>
          </tbody>
        </table>
      </section>

      <section className="panel" aria-label={t('na.detailResource')}>
        <h3>{t('na.detailResource')}</h3>
        <table>
          <tbody>
            <tr><th>{t('na.detailResource')}</th><td className="dim">{detail.resource_id}</td></tr>
            <tr><th>{t('na.engagement')}</th><td className="dim">{detail.engagement_id}</td></tr>
            <tr><th className="num">{t('col.requested')}</th><td className="num dim">{fmtTokens(detail.requested_tokens)}</td></tr>
          </tbody>
        </table>
      </section>

      <section className="panel" aria-label={t('na.detailRooms')}>
        <h3>{t('na.detailRooms')}</h3>
        {detail.rooms.length === 0 ? (
          <p className="dim">{t('na.noRooms')}</p>
        ) : (
          <table>
            <thead>
              <tr><th>{t('na.detailRooms')}</th><th>{t('col.state')}</th></tr>
            </thead>
            <tbody>
              {detail.rooms.map((room) => (
                <tr key={room.session_id} data-session-id={room.session_id}>
                  <td className="dim">{room.room_id}</td>
                  <td>{room.dispatch_state ?? '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section className="panel" aria-label={t('na.detailDispatch')}>
        <h3>{t('na.detailDispatch')}</h3>
        {detail.dispatch === null ? (
          <p className="dim">{t('na.detailNoDispatch')}</p>
        ) : (
          <table>
            <tbody>
              <tr><th>{t('col.state')}</th><td>{detail.dispatch.dispatch_state}</td></tr>
              <tr><th>{t('na.detailDispatch')}</th><td className="dim">{detail.dispatch.dispatch_id}</td></tr>
              <tr><th>{t('na.detailRooms')}</th><td className="dim">{detail.dispatch.room_id}</td></tr>
            </tbody>
          </table>
        )}
      </section>

      <section className="panel" aria-label={t('na.detailTasks')}>
        <h3>{t('na.detailTasks')}</h3>
        {detail.tasks.length === 0 ? (
          <p className="dim">{t('na.detailNoTasks')}</p>
        ) : (
          <table>
            <thead>
              <tr><th>{t('na.detailTasks')}</th><th>{t('col.state')}</th><th>{t('na.lastActivity')}</th></tr>
            </thead>
            <tbody>
              {detail.tasks.map((task) => (
                <tr key={task.id} data-task-id={task.id}>
                  <td>{task.title}</td>
                  <td>{task.status}</td>
                  <td className="dim">{new Date(task.updated_at).toISOString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section className="panel" aria-label={t('na.detailReminders')}>
        <h3>{t('na.detailReminders')}</h3>
        {detail.reminders.length === 0 ? (
          <p className="dim">{t('na.detailNoReminders')}</p>
        ) : (
          <table>
            <thead>
              <tr><th>{t('na.detailReminders')}</th><th>{t('na.reminderFiresAt')}</th><th>{t('na.reminderState')}</th></tr>
            </thead>
            <tbody>
              {detail.reminders.map((reminder) => (
                <tr key={reminder.id} data-reminder-id={reminder.id}>
                  <td>{reminder.msg}</td>
                  <td className="dim">{new Date(reminder.fire_at).toISOString()}</td>
                  <td>{reminder.fired_at === null ? t('na.reminderPending') : t('na.reminderFired')}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <div className="btn-row" style={{ marginTop: 14 }}>
        {back}
      </div>

      {/* Board #49 controls: each panel owns its own read/write, so a
       * failure in one never blanks the others or the detail above. The
       * mutations (avatar, undelete) follow the retained console's own
       * rule — a read-only session sees the read (delivery events) but no
       * control it could only have refused. */}
      <LaunchEnv name={name} />
      <DeliveryEvents name={name} />
      {manageLifecycle && !removed && <AvatarControl name={name} />}
      {manageLifecycle && !removed && <UndeleteControl name={name} />}

      {removed && (
        <section className="notice" data-agent-removed="true" role="status">
          <p>{t('na.deleted', { name })}</p>
        </section>
      )}

      {manageLifecycle && !removed && (
        <div className="danger-zone">
          <span className="lbl">{t('na.deleteAgent')}</span>
          {confirming === null && (
            <button className="btn danger" disabled={removing?.kind === 'pending'} onClick={() => { setConfirming(true); setTyped(''); }}>
              {t('na.deleteAgent')}
            </button>
          )}
          {confirming && (
            <>
              <span className="dim">{t('na.deleteConfirm', { name })}</span>
              <input
                value={typed}
                onChange={(event) => setTyped(event.target.value)}
                aria-label={t('na.deleteConfirm', { name })}
                placeholder={name}
              />
              <button className="btn" onClick={() => { setConfirming(null); setTyped(''); }}>{t('act.cancel')}</button>
              <button className="btn danger" disabled={typed !== name || removing?.kind === 'pending'} onClick={() => void remove()}>
                {t('na.deletePermanently')}
              </button>
            </>
          )}
        </div>
      )}
      {!manageLifecycle && <p className="dim" style={{ marginTop: 14 }}>{t('na.deleteLiveOnly')}</p>}
      {removing && removing.kind !== 'pending' && (
        <section className="notice" data-delete-action={removing.kind} role="alert">
          <p>{t(`na.delete.${removing.kind}`)}</p>
        </section>
      )}
    </div>
  );
}
