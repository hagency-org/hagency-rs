//! Finite in-memory browser authority, distinct from the operator credential.
use super::Error;
use hagency_core::allocation::Ceiling;
use hagency_store::{
    AccountEnrollmentAccess, AccountEnrollmentCommand, ManagedAccount, ResourceConfigurationAccess,
    ResourceConfigurationCommand, ResourcePublicationAccess, ResourcePublicationCommand,
    ResourcePublicationRetirement,
};
use sha2::{Digest, Sha256};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

/// The operator's personal console: neither the access link nor a login
/// expires on a timer, and both survive a restart (their SHA-256 hashes live
/// in this private file in the state directory). A new link replaces the old
/// one; logout ends a login.
const LOGINS_FILE: &str = "console-logins.json";
const LOGINS_MAX_BYTES: u64 = 64 * 1024;
/// TS parity (`createApiAuthMiddleware`, lib/backend/auth-adapter.js:312-326):
/// a login never expires on a timer — the token authenticates every request
/// for the life of the process. The store-side mutation gates still need a
/// finite `expires`, so one login carries this horizon; logout and process
/// retirement are the real bounds.
const ACCESS_HORIZON: Duration = Duration::from_secs(365 * 24 * 60 * 60);
pub(super) const COOKIE: &str = "hagency_console";

/// One login's full authority: the TS middleware admitted every `/api`
/// action to one credential, so every session owns every mutation gate.
struct Access {
    publication: ResourcePublicationAccess,
    configuration: ResourceConfigurationAccess,
    account: AccountEnrollmentAccess,
}
impl Access {
    fn revoke(&self) -> Result<(), hagency_store::Error> {
        self.publication.revoke()?;
        self.configuration.revoke()?;
        self.account.revoke()
    }
}
struct Grant {
    hash: [u8; 32],
    /// `None` on a session: login survives reloads and time, ending only at
    /// logout or process retirement. The ticket keeps its link freshness.
    expires: Option<Instant>,
    access: Option<Access>,
}
struct State {
    retired: bool,
    ticket: Option<Grant>,
    sessions: Vec<Grant>,
}
pub(super) struct Authority(
    Mutex<State>,
    ResourcePublicationRetirement,
    Option<std::path::PathBuf>,
);
pub(super) struct Session([u8; 32]);
/// The bounded enrolment inputs, grouped so the authority's constructor
/// stays reviewable — the request's five scalars travel as one value
/// (the hosted lanes' `too_many_arguments` limit is a shape signal, not
/// a lint to allow).
pub(super) struct AccountEnrollmentInput {
    pub(super) revision: String,
    pub(super) model: String,
    pub(super) reasoning: Option<String>,
    pub(super) ceiling: Option<Ceiling>,
    pub(super) deadline: Instant,
}

fn secret() -> Result<String, Error> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn hash(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(Error::Unauthorized);
    }
    Ok(Sha256::digest(value.as_bytes()).into())
}
fn fresh(grant: &Grant, now: Instant) -> bool {
    grant.expires.is_none_or(|expires| now < expires)
}
fn matches(grant: &Grant, digest: &[u8; 32], now: Instant) -> bool {
    bool::from(grant.hash.ct_eq(digest)) && fresh(grant, now)
}
/// The persisted link and login hashes; `None` when absent or unreadable.
#[allow(clippy::type_complexity)]
fn load(path: &std::path::Path) -> Option<(Option<[u8; 32]>, Vec<[u8; 32]>)> {
    use std::io::Read;
    let file = hagency_store::private::open(path, false).ok()?;
    if file.metadata().ok()?.len() > LOGINS_MAX_BYTES {
        return None;
    }
    let mut text = String::new();
    (&file).read_to_string(&mut text).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let parse = |v: &serde_json::Value| -> Option<[u8; 32]> {
        let hex = v.as_str()?;
        if hex.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(out)
    };
    let ticket = match &value["ticket"] {
        serde_json::Value::Null => None,
        v => Some(parse(v)?),
    };
    let sessions = value["sessions"]
        .as_array()?
        .iter()
        .take(64)
        .map(parse)
        .collect::<Option<Vec<_>>>()?;
    Some((ticket, sessions))
}
impl Authority {
    pub(super) fn new() -> Self {
        Self(
            Mutex::new(State {
                retired: false,
                ticket: None,
                sessions: Vec::new(),
            }),
            ResourcePublicationRetirement::default(),
            None,
        )
    }
    /// The production authority: the link and every login are reloaded from
    /// the state directory, so a restart signs nobody out. An unreadable file
    /// starts empty (the operator mints a new link), never refuses startup.
    pub(super) fn persistent(state_dir: &std::path::Path) -> Self {
        let path = state_dir.join(LOGINS_FILE);
        let retirement = ResourcePublicationRetirement::default();
        let (ticket, sessions) = load(&path).unwrap_or_default();
        let until = Instant::now() + ACCESS_HORIZON;
        let sessions = sessions
            .into_iter()
            .map(|hash| Grant {
                hash,
                expires: None,
                access: Some(Access {
                    publication: ResourcePublicationAccess::new(until, retirement.clone()),
                    configuration: ResourceConfigurationAccess::new(until, retirement.clone()),
                    account: AccountEnrollmentAccess::new(until, retirement.clone()),
                }),
            })
            .collect();
        Self(
            Mutex::new(State {
                retired: false,
                ticket: ticket.map(|hash| Grant {
                    hash,
                    expires: None,
                    access: None,
                }),
                sessions,
            }),
            retirement,
            Some(path),
        )
    }
    /// Write the current link and logins (hashes only). Called with the
    /// state lock held, after every change.
    fn save(&self, state: &State) -> Result<(), Error> {
        let Some(path) = &self.2 else {
            return Ok(());
        };
        let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let value = serde_json::json!({
            "ticket": state.ticket.as_ref().map(|t| hex(&t.hash)),
            "sessions": state.sessions.iter().map(|s| hex(&s.hash)).collect::<Vec<_>>(),
        });
        hagency_store::private::replace(path, value.to_string().as_bytes())
            .map_err(|_| Error::Unavailable)
    }
    /// TS parity (lib/backend/auth-adapter.js:312-326): issuing access is
    /// handing the operator the credential — never rate-limited, never
    /// scope-selected. A fresh invocation replaces any unexchanged link.
    pub(super) fn issue(&self) -> Result<String, Error> {
        self.issue_with(Instant::now)
    }
    #[cfg(test)]
    fn issue_at(&self, now: Instant) -> Result<String, Error> {
        self.issue_with(|| now)
    }
    fn issue_with(&self, clock: impl FnOnce() -> Instant) -> Result<String, Error> {
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        let now = clock();
        if state.retired {
            return Err(Error::Unavailable);
        }
        let _ = now;
        let value = secret()?;
        state.ticket = Some(Grant {
            hash: hash(&value)?,
            expires: None,
            access: None,
        });
        self.save(&state)?;
        Ok(value)
    }
    pub(super) fn exchange(&self, ticket: &str) -> Result<String, Error> {
        self.exchange_with(ticket, Instant::now)
    }
    #[cfg(test)]
    fn exchange_at(&self, ticket: &str, now: Instant) -> Result<String, Error> {
        self.exchange_with(ticket, || now)
    }
    /// The ticket is a plain credential, not a one-time token: exchanging
    /// it again yields another logged-in session (a reload of the access
    /// link never meets a burn). Every session carries the FULL access —
    /// one login, every console action, the TS middleware's shape.
    fn exchange_with(
        &self,
        ticket: &str,
        clock: impl FnOnce() -> Instant,
    ) -> Result<String, Error> {
        let digest = hash(ticket)?;
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        let now = clock();
        if state.retired {
            return Err(Error::Unavailable);
        }
        if !state
            .ticket
            .as_ref()
            .is_some_and(|t| matches(t, &digest, now))
        {
            return Err(Error::Unauthorized);
        }
        let value = secret()?;
        let until = now + ACCESS_HORIZON;
        // A bound, not a rate limit: the browser keeps only its newest
        // cookie, so superseded grants are garbage. Dropping the OLDEST
        // keeps memory finite without ever refusing an exchange — a reload
        // can never meet a 429 here.
        while state.sessions.len() >= 64 {
            let removed = state.sessions.remove(0);
            if let Some(access) = removed.access {
                let _ = access.revoke();
            }
        }
        state.sessions.push(Grant {
            hash: hash(&value)?,
            expires: None,
            access: Some(Access {
                publication: ResourcePublicationAccess::new(until, self.1.clone()),
                configuration: ResourceConfigurationAccess::new(until, self.1.clone()),
                account: AccountEnrollmentAccess::new(until, self.1.clone()),
            }),
        });
        self.save(&state)?;
        Ok(value)
    }
    pub(super) fn authenticate(&self, value: &str) -> Result<Session, Error> {
        let session = Session(hash(value)?);
        self.check(&session)?;
        Ok(session)
    }
    pub(super) fn check(&self, session: &Session) -> Result<(), Error> {
        self.check_with(session, Instant::now)
    }
    #[cfg(test)]
    fn check_at(&self, session: &Session, now: Instant) -> Result<(), Error> {
        self.check_with(session, || now)
    }
    fn check_with(&self, session: &Session, clock: impl FnOnce() -> Instant) -> Result<(), Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        let now = clock();
        if state.retired {
            return Err(Error::Unavailable);
        }
        if state.sessions.iter().any(|s| matches(s, &session.0, now)) {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }
    pub(super) fn revoke(&self, session: &Session) -> Result<(), Error> {
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if let Some(access) = state
            .sessions
            .iter()
            .find(|s| bool::from(s.hash.ct_eq(&session.0)))
            .and_then(|s| s.access.as_ref())
        {
            access.revoke().map_err(|e| {
                if matches!(e, hagency_store::Error::Busy) {
                    Error::Busy
                } else {
                    Error::Unavailable
                }
            })?;
        }
        state
            .sessions
            .retain(|s| !bool::from(s.hash.ct_eq(&session.0)));
        self.save(&state)
    }
    pub(super) fn retire(&self) {
        self.1.retire();
        if let Ok(mut state) = self.0.lock() {
            state.retired = true;
            state.ticket = None;
            state.sessions.clear();
        }
    }
    fn logged_in(&self, session: &Session) -> Result<bool, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        Ok(state.sessions.iter().any(|s| matches(s, &session.0, now)))
    }
    /// One login is the whole console (TS parity): the permission envelope
    /// answers true for every action class a logged-in session asks about.
    pub(super) fn can_publish(&self, session: &Session) -> Result<bool, Error> {
        self.logged_in(session)
    }
    pub(super) fn can_configure(&self, session: &Session) -> Result<bool, Error> {
        self.logged_in(session)
    }
    pub(super) fn contribution(
        &self,
        session: &Session,
        grant: hagency_store::coordinator::ResourceGrant,
    ) -> Result<hagency_store::coordinator::ResourceContributionCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .configuration
            .prepare_contribution(grant, now + Duration::from_secs(15))
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    pub(super) fn delegation(
        &self,
        session: &Session,
        change: hagency_store::coordinator::DelegationChange,
    ) -> Result<hagency_store::coordinator::DelegationCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .configuration
            .prepare_delegation(change, now + Duration::from_secs(15))
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    pub(super) fn settlement(
        &self,
        session: &Session,
        change: hagency_store::coordinator::FinalUsage,
    ) -> Result<hagency_store::coordinator::SettlementCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .configuration
            .prepare_settlement(change, now + Duration::from_secs(15))
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    pub(super) fn can_manage_accounts(&self, session: &Session) -> Result<bool, Error> {
        self.logged_in(session)
    }
    pub(super) fn can_lifecycle(&self, session: &Session) -> Result<bool, Error> {
        self.logged_in(session)
    }
    pub(super) fn account(
        &self,
        session: &Session,
        managed: &ManagedAccount,
        input: AccountEnrollmentInput,
    ) -> Result<AccountEnrollmentCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .account
            .prepare(
                managed,
                input.revision,
                input.model,
                input.reasoning,
                input.ceiling,
                input.deadline,
            )
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    pub(super) fn configuration(
        &self,
        session: &Session,
        input: super::resource_configuration::PreparedInput,
        deadline: Instant,
    ) -> Result<ResourceConfigurationCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .configuration
            .prepare(
                input.resource,
                input.revision,
                input.create,
                input.profile,
                input.ceiling,
                deadline,
            )
            .and_then(|command| match input.engagement {
                Some(change) => command.with_engagement(change),
                None => Ok(command),
            })
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
    pub(super) fn publication(
        &self,
        session: &Session,
        resource: String,
        revision: String,
        published: bool,
        deadline: Instant,
    ) -> Result<ResourcePublicationCommand, Error> {
        let state = self.0.lock().map_err(|_| Error::Unavailable)?;
        if state.retired {
            return Err(Error::Unavailable);
        }
        let now = Instant::now();
        let access = state
            .sessions
            .iter()
            .find(|s| matches(s, &session.0, now))
            .and_then(|s| s.access.as_ref())
            .ok_or(Error::Unauthorized)?;
        access
            .publication
            .prepare(resource, revision, published, deadline)
            .map_err(|e| match e {
                hagency_store::Error::Busy => Error::Busy,
                hagency_store::Error::LocalAuthority => Error::Unauthorized,
                hagency_store::Error::Invalid(_) => Error::Invalid,
                _ => Error::Unavailable,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The personal console survives a restart: the link and the login are
    /// reloaded from the state directory; only a new link or logout ends them.
    #[test]
    fn native_console_logins_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = Authority::persistent(dir.path());
        let ticket = first.issue().unwrap();
        let cookie = first.exchange(&ticket).unwrap();
        first.retire();
        drop(first);
        let second = Authority::persistent(dir.path());
        let session = second.authenticate(&cookie).unwrap();
        assert!(second.can_lifecycle(&session).unwrap());
        let again = second.exchange(&ticket).unwrap();
        second.revoke(&session).unwrap();
        drop(second);
        let third = Authority::persistent(dir.path());
        assert!(matches!(
            third.authenticate(&cookie),
            Err(Error::Unauthorized)
        ));
        third.authenticate(&again).unwrap();
    }
    /// TS parity: the ticket is a reusable credential and the login it
    /// produces is never rate-limited, never capped and never expires.
    #[test]
    fn native_console_login_is_reusable() {
        let authority = Authority::new();
        let now = Instant::now();
        let ticket = authority.issue_at(now).unwrap();
        // Handing the credential out again is the same login, never a 429:
        // a fresh link simply replaces the unexchanged one.
        let second = authority.issue_at(now).unwrap();
        assert_ne!(ticket, second);
        assert!(
            authority.exchange_at(&ticket, now).is_err(),
            "replaced link is retired"
        );
        // The surviving link exchanges ANY number of times — a reload of
        // the access URL never meets a one-time burn.
        for at in [
            now,
            now + Duration::from_secs(1),
            now + Duration::from_secs(2),
        ] {
            let cookie = authority.exchange_at(&second, at).unwrap();
            authority
                .check_at(&Session(hash(&cookie).unwrap()), at)
                .unwrap();
        }
        // The link has no timer either: it still signs in a year later.
        authority
            .exchange_at(&second, now + Duration::from_secs(365 * 24 * 60 * 60))
            .unwrap();
        let ticket = authority.issue_at(now + Duration::from_secs(1)).unwrap();
        let cookie = authority
            .exchange_at(&ticket, now + Duration::from_secs(1))
            .unwrap();
        let session = Session(hash(&cookie).unwrap());
        // The session has NO timer: a reload after any delay still works.
        authority
            .check_at(&session, now + Duration::from_secs(100 * 24 * 60 * 60))
            .unwrap();
        assert!(authority.can_publish(&session).unwrap());
        assert!(authority.can_configure(&session).unwrap());
        assert!(authority.can_manage_accounts(&session).unwrap());
        assert!(authority.can_lifecycle(&session).unwrap());
        // Logout ends it — the bound TS parity keeps.
        authority.revoke(&session).unwrap();
        assert!(matches!(
            authority.check(&session),
            Err(Error::Unauthorized)
        ));
        authority.retire();
        assert!(authority.issue().is_err());
    }

    /// One login carries every action class (the TS middleware admitted one
    /// credential to every route); logout revokes them all at once.
    #[test]
    fn native_console_one_login_full_authority() {
        let authority = Authority::new();
        let now = Instant::now();
        let ticket = authority.issue_at(now).unwrap();
        let cookie = authority.exchange_at(&ticket, now).unwrap();
        let session = Session(hash(&cookie).unwrap());
        let publication = authority
            .publication(
                &session,
                "resource".into(),
                "a".repeat(64),
                false,
                now + Duration::from_secs(2),
            )
            .unwrap();
        // A pending command keeps logout busy (busy is not revocation).
        assert!(matches!(authority.revoke(&session), Err(Error::Busy)));
        drop(publication);
        let input = || super::super::resource_configuration::PreparedInput {
            resource: "resource".into(),
            revision: "a".repeat(64),
            create: false,
            profile: hagency_store::ProfileChange::Preserve {},
            ceiling: hagency_store::CeilingChange::Preserve {},
            engagement: None,
        };
        let configuration = authority
            .configuration(&session, input(), now + Duration::from_secs(2))
            .unwrap();
        drop(configuration);
        authority.revoke(&session).unwrap();
        assert!(matches!(
            authority.check_at(&session, now + Duration::from_secs(3)),
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            authority.publication(
                &session,
                "resource".into(),
                "a".repeat(64),
                false,
                now + Duration::from_secs(4),
            ),
            Err(Error::Unauthorized)
        ));
    }

    #[test]
    fn native_console_clock_after_lock() {
        use std::sync::{Arc, mpsc};
        //
        // Expiry is read AFTER the authority mutex is taken, so a grant
        // expiring while the lock is held still refuses (the clock is not
        // sampled before the critical section).
        let ordered = Authority::new();
        let clock = std::sync::Mutex::new(Instant::now());
        let ticket = ordered.issue_with(|| *clock.lock().unwrap()).unwrap();
        let cookie = ordered
            .exchange_with(&ticket, || *clock.lock().unwrap())
            .unwrap();
        ordered
            .check_with(&Session(hash(&cookie).unwrap()), || *clock.lock().unwrap())
            .unwrap();
        for ticket_check in [false, true] {
            let authority = Arc::new(Authority::new());
            let ticket = authority.issue().unwrap();
            let credential = if ticket_check {
                ticket
            } else {
                authority.exchange(&ticket).unwrap()
            };
            let mut held = authority.0.lock().unwrap();
            let expires = Instant::now() + Duration::from_millis(80);
            if ticket_check {
                held.ticket.as_mut().unwrap().expires = Some(expires);
            } else {
                held.sessions[0].expires = Some(expires);
            }
            let (began, entered) = mpsc::channel();
            let other = authority.clone();
            let call = std::thread::spawn(move || {
                began.send(()).unwrap();
                if ticket_check {
                    other.exchange(&credential).map(|_| ())
                } else {
                    other.authenticate(&credential).map(|_| ())
                }
            });
            entered.recv().unwrap();
            std::thread::sleep(
                expires.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
            );
            drop(held);
            assert!(matches!(call.join().unwrap(), Err(Error::Unauthorized)));
        }
    }
}
