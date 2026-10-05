//! Agent identity polish (board #11, parity with the retained bridge):
//! display-name reconciliation, approval-DM power levels, the owner-absent
//! warning text, and the send-retry warning — the exact retained shapes as
//! pure functions, so the wiring points stay one-liners.
//!
//! TS anchors: lib/matrix-agent-profile.js:1-30 (display name),
//! bridge-matrix.js:2561-2576 (`approvalRoomPowerLevels`),
//! :8861-8887 (`ensureApprovalDmRestricted` normalize+compare),
//! :9267-9297 (`warnIfOwnerCannotSeeApprovalRoom`),
//! :10888-10950 (membership failure → re-invite, rejoin, resend).

use crate::{CancellationToken, Error, http::Http};
use serde_json::{Value, json};

/// The retained bridge's per-agent profile-reconcile throttle
/// (`reconcileAgentProfile`, bridge-matrix.js:5938-5940): `Date.now() - last
/// < 300_000` returns early, so at most one display-name reconcile per agent
/// every 5 minutes.
pub const PROFILE_RECONCILE_INTERVAL_MS: u64 = 300_000;

/// The machine-generated names a reconciliation may overwrite
/// (lib/matrix-agent-profile.js:22): a user's custom profile always wins.
pub fn is_machine_generated(current: &str, mxid: &str, agent_name: &str) -> bool {
    let localpart = &mxid[1..mxid.find(':').unwrap_or(mxid.len())];
    [mxid, localpart, agent_name, &format!("🤖 {agent_name}")].contains(&current)
}

/// The desired display name (lib/matrix-agent-profile.js:6-8): trimmed,
/// capped at 128 characters, never empty.
pub fn desired_display_name(display_name: &str) -> Option<String> {
    let desired = display_name.trim().chars().take(128).collect::<String>();
    (!desired.is_empty()).then_some(desired)
}

/// The approval-room power levels (bridge-matrix.js:2561-2576), verbatim:
/// everything that mutates the room sits at 100, only the room's own
/// creator-actor holds it, and message events stay open (events_default 0)
/// so the owner can still talk.
pub fn approval_room_power_levels(actor_mxid: &str) -> Result<Value, Error> {
    if !actor_mxid.starts_with('@') || !actor_mxid.contains(':') {
        return Err(Error::Config);
    }
    Ok(json!({
        "ban": 100,
        "events_default": 0,
        "invite": 100,
        "kick": 100,
        "notifications": {"room": 100},
        "redact": 100,
        "state_default": 100,
        "users": {actor_mxid: 100},
        "users_default": 0,
    }))
}

/// The normalized comparison subset (bridge-matrix.js:8878-8886): exactly the
/// ten keys `approvalRoomPowerLevels` owns — a current state carrying any
/// other key compares equal on these and is left alone; a difference on any
/// of them is a difference.
#[cfg(test)]
pub fn normalized_power_levels(current: &Value) -> Value {
    json!({
        "ban": current.get("ban").cloned().unwrap_or(Value::Null),
        "events_default": current.get("events_default").cloned().unwrap_or(Value::Null),
        "invite": current.get("invite").cloned().unwrap_or(Value::Null),
        "kick": current.get("kick").cloned().unwrap_or(Value::Null),
        "notifications": current.get("notifications").cloned().unwrap_or(Value::Null),
        "redact": current.get("redact").cloned().unwrap_or(Value::Null),
        "state_default": current.get("state_default").cloned().unwrap_or(Value::Null),
        "users": current.get("users").cloned().unwrap_or(Value::Null),
        "users_default": current.get("users_default").cloned().unwrap_or(Value::Null),
    })
}

/// Whether the room's power levels must be (re)written
/// (bridge-matrix.js:8877-8887): absent state, or a normalized difference.
#[cfg(test)]
pub fn power_levels_differ(current: Option<&Value>, expected: &Value) -> bool {
    match current {
        None => true,
        Some(current) => normalized_power_levels(current) != *expected,
    }
}

/// The owner-absent warning (bridge-matrix.js:9279-9283), verbatim words.
pub fn owner_absent_warning(agent: Option<&str>, room_id: &str, owner: &str) -> String {
    format!(
        "approval request for {} was delivered to {}, but its owner {} is NOT in that room \
         — invited and never joined, or since departed. Nobody who can decide will see it. \
         Remedy: have the owner accept the invitation to that room, or point \
         HAGENCY_OWNER_DM_ROOM (or the binding) at a room they are actually in.",
        agent.unwrap_or("an agent"),
        room_id,
        owner
    )
}

/// The send-retry warning (bridge-matrix.js:10946-10948), verbatim words.
pub fn send_retry_warning(room_id: &str, reason: &str) -> String {
    format!("sendAsAgent failed in room {room_id} (after auto-join retry): {reason}")
}

/// What the representative can do in a room (lib/matrix-representative.js:1005-1030):
/// `required = invite ?? 0`, `mine = users[actor] ?? users_default`, `can = mine >= required`.
///
/// A read that did not establish the answer is `known: false` — never a guess.
/// `backend-v2.js:14492` says why: "A guess here would be worse than the bare
/// error: it would name a cause we did not establish."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvitePower {
    pub known: bool,
    pub can: bool,
    pub required: u64,
    pub mine: u64,
}

/// Classify a power-levels read for `actor_mxid`. `None`, or a value that is not
/// an object, is the unreadable case: `known: false`, and the caller leaves the
/// original refusal alone.
pub fn representative_invite_power(power_levels: Option<&Value>, actor_mxid: &str) -> InvitePower {
    let unreadable = InvitePower {
        known: false,
        can: false,
        required: 0,
        mine: 0,
    };
    let Some(levels) = power_levels.and_then(Value::as_object) else {
        return unreadable;
    };
    let field = |key: &str| levels.get(key).and_then(Value::as_u64).unwrap_or(0);
    let required = field("invite");
    let mine = levels
        .get("users")
        .and_then(Value::as_object)
        .and_then(|users| users.get(actor_mxid))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| field("users_default"));
    InvitePower {
        known: true,
        can: mine >= required,
        required,
        mine,
    }
}

/// The remedy that belongs to the PROJECT (backend-v2.js:14477-14480), verbatim.
/// Naming it matters: a bare 403 sends an operator to check the credential, which
/// is the one thing that was working.
pub fn invite_power_remedy(mine: u64, required: u64, room_id: &str, agent_mxid: &str) -> String {
    format!(
        "our representative holds power {mine} in {room_id} and inviting needs {required}. \
         The project side either grants it that power or invites {agent_mxid} itself; \
         nothing on our side can raise it."
    )
}

/// The owner-membership verdict (`bridge-matrix.js:9271-9292`). Present ⇒
/// silence; absent ⇒ one warning; unreadable ⇒ silence, because "I could not ask"
/// is not "the owner is absent" (`:9264-9265`) and conflating the two would make
/// the alert untrustworthy the first time a homeserver was slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerVerdict {
    Present,
    Absent,
    Unreadable,
}

/// Classify a joined-member read for `owner_mxid` (`bridge-matrix.js:9271-9292`).
/// The comparison is case-insensitive because Matrix localparts are (`:9289`),
/// and a read that produced NO member list says nothing rather than reporting an
/// absence (`:9264`).
pub fn owner_membership_verdict(joined: Option<&[String]>, owner_mxid: &str) -> OwnerVerdict {
    let Some(joined) = joined else {
        return OwnerVerdict::Unreadable;
    };
    let owner = owner_mxid.to_lowercase();
    if joined.iter().any(|m| m.to_lowercase() == owner) {
        OwnerVerdict::Present
    } else {
        OwnerVerdict::Absent
    }
}

/// The agent's own rejoin (bridge-matrix.js:10936-10943, the join half of the
/// retained invite-then-join pair): POST /join/{roomId} as the agent. A kicked
/// member needs a fresh invite no agent can mint for itself — the caller
/// surfaces the warning and keeps the failure non-terminal when this refuses.
pub async fn agent_rejoin(
    http: &Http,
    room_id: &str,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let response = http
        .post(
            &["_matrix", "client", "v3", "join", room_id],
            "{}".to_owned(),
            cancel,
        )
        .await?;
    response.success()?;
    Ok(())
}

/// Display-name reconciliation (lib/matrix-agent-profile.js:3-30): GET the
/// current name, overwrite only a machine-generated one, PUT, read back. A
/// custom name wins silently (`changed: false, custom: true`); a mismatched
/// readback is an error, never a silent no-op.
pub async fn reconcile_display_name(
    http: &Http,
    mxid: &str,
    agent_name: &str,
    display_name: &str,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    reconcile_name(http, mxid, Some(agent_name), display_name, cancel).await
}

/// A scoped, durable owner/coordinator rename explicitly replaces an existing
/// custom label. Success requires reading back the exact requested label.
pub async fn apply_display_name(
    http: &Http,
    mxid: &str,
    display_name: &str,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    reconcile_name(http, mxid, None, display_name, cancel).await
}

async fn reconcile_name(
    http: &Http,
    mxid: &str,
    automatic_agent_name: Option<&str>,
    display_name: &str,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    let Some(desired) = desired_display_name(display_name) else {
        return Ok(false);
    };
    let read = http
        .request(
            &["_matrix", "client", "v3", "profile", mxid, "displayname"],
            None,
            cancel,
        )
        .await?
        .success()?;
    let current = read
        .get("displayname")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if current == desired {
        return Ok(false);
    }
    if !current.is_empty()
        && automatic_agent_name
            .is_some_and(|agent_name| !is_machine_generated(current, mxid, agent_name))
    {
        return Ok(false);
    }
    http.put(
        &["_matrix", "client", "v3", "profile", mxid, "displayname"],
        json!({"displayname": desired}).to_string(),
        cancel,
    )
    .await?
    .success()?;
    let readback = http
        .request(
            &["_matrix", "client", "v3", "profile", mxid, "displayname"],
            None,
            cancel,
        )
        .await?
        .success()?;
    if readback.get("displayname").and_then(Value::as_str) != Some(desired.as_str()) {
        return Err(Error::Wire);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_generated_names_are_exactly_the_retained_four() {
        let mxid = "@agent_project_request:example.test";
        assert!(is_machine_generated(mxid, mxid, "UsageWorker"));
        assert!(is_machine_generated(
            "agent_project_request",
            mxid,
            "UsageWorker"
        ));
        assert!(is_machine_generated("UsageWorker", mxid, "UsageWorker"));
        assert!(is_machine_generated("🤖 UsageWorker", mxid, "UsageWorker"));
        assert!(!is_machine_generated("Ada's agent", mxid, "UsageWorker"));
        assert!(!is_machine_generated("", mxid, "UsageWorker"));
    }

    #[test]
    fn desired_name_trims_and_caps_at_128_scalars() {
        assert_eq!(desired_display_name("  Vivian  "), Some("Vivian".into()));
        assert_eq!(desired_display_name("   "), None);
        assert_eq!(
            desired_display_name(&"𝕏".repeat(200)).map(|s| s.chars().count()),
            Some(128)
        );
    }

    #[test]
    fn power_levels_match_the_retained_shape() {
        let expected = approval_room_power_levels("@agent:example.test").unwrap();
        assert_eq!(
            expected,
            json!({
                "ban": 100, "events_default": 0, "invite": 100, "kick": 100,
                "notifications": {"room": 100}, "redact": 100, "state_default": 100,
                "users": {"@agent:example.test": 100}, "users_default": 0,
            })
        );
        assert!(approval_room_power_levels("agent:example.test").is_err());
        // Equal on the ten owned keys: no rewrite.
        assert!(!power_levels_differ(Some(&expected), &expected));
        // Any owned-key drift or absent state: rewrite.
        let mut drifted = expected.clone();
        drifted["invite"] = json!(0);
        assert!(power_levels_differ(Some(&drifted), &expected));
        assert!(power_levels_differ(None, &expected));
        // A foreign key compares on the owned subset only.
        let mut extra = expected.clone();
        extra["history_visibility"] = json!("shared");
        assert!(!power_levels_differ(Some(&extra), &expected));
    }

    /// TS `api-engagement-room-admission.test.js:537-556`: too little power is
    /// named, with the remedy that belongs to the project.
    #[test]
    fn invite_power_classifies_and_names_the_project_remedy() {
        // The test's own fixture: invite 50, users_default 0, representative absent.
        let levels =
            json!({"invite": 50, "users_default": 0, "users": {"@someone:palpo.test": 100}});
        let power = representative_invite_power(Some(&levels), "@hagency:palpo.test");
        assert!(power.known && !power.can);
        assert_eq!((power.mine, power.required), (0, 50));
        let remedy = invite_power_remedy(
            power.mine,
            power.required,
            "!room:palpo.test",
            "@ac_x:palpo.test",
        );
        assert!(remedy.contains("holds power 0"), "{remedy}");
        assert!(remedy.contains("needs 50"), "{remedy}");
        assert!(
            remedy.contains("grants it that power or invites"),
            "{remedy}"
        );
        assert!(
            remedy.contains("nothing on our side can raise it"),
            "{remedy}"
        );

        // `:561` — with enough power the diagnosis must NOT fire: same 403,
        // different problem, and mislabelling it sends the project to change a
        // setting that is already right.
        let enough =
            json!({"invite": 50, "users_default": 0, "users": {"@hagency:palpo.test": 50}});
        assert!(representative_invite_power(Some(&enough), "@hagency:palpo.test").can);

        // `:577` — an unreadable read names no cause.
        let unreadable = representative_invite_power(None, "@hagency:palpo.test");
        assert!(!unreadable.known, "\"I could not ask\" is not a verdict");
        assert!(unreadable.required == 0 && unreadable.mine == 0);
        // Absent `users_default` falls back to 0, as the TS `?? 0` does.
        let bare = representative_invite_power(Some(&json!({"invite": 50})), "@hagency:palpo.test");
        assert!(bare.known && !bare.can && bare.mine == 0);
    }

    /// TS `approval-owner-can-see-it.test.js:82-165`: present ⇒ silence; absent ⇒
    /// the warning; unreadable ⇒ silence, and the mxid comparison is
    /// case-insensitive.
    #[test]
    fn owner_membership_verdict_keeps_the_retained_three_way() {
        let present = vec![
            "@owner:example.test".to_owned(),
            "@other:example.test".to_owned(),
        ];
        assert_eq!(
            owner_membership_verdict(Some(&present), "@owner:example.test"),
            OwnerVerdict::Present
        );
        // `:103` — Matrix localparts are case-insensitive, so the comparison is.
        assert_eq!(
            owner_membership_verdict(Some(&present), "@OWNER:EXAMPLE.TEST"),
            OwnerVerdict::Present
        );
        let absent = vec!["@someone:else.test".to_owned()];
        assert_eq!(
            owner_membership_verdict(Some(&absent), "@owner:example.test"),
            OwnerVerdict::Absent
        );
        // `:110` — a read that produced no member list says nothing.
        assert_eq!(
            owner_membership_verdict(None, "@owner:example.test"),
            OwnerVerdict::Unreadable
        );
    }

    #[test]
    fn owner_absent_warning_keeps_the_retained_words() {
        assert_eq!(
            owner_absent_warning(Some("worker"), "!dm:example.test", "@owner:example.test"),
            "approval request for worker was delivered to !dm:example.test, but its owner \
             @owner:example.test is NOT in that room — invited and never joined, or since \
             departed. Nobody who can decide will see it. Remedy: have the owner accept the \
             invitation to that room, or point HAGENCY_OWNER_DM_ROOM (or the binding) at a \
             room they are actually in."
        );
        assert!(
            owner_absent_warning(None, "!dm:example.test", "@owner:example.test")
                .starts_with("approval request for an agent was delivered to")
        );
    }

    #[test]
    fn send_retry_warning_keeps_the_retained_words() {
        assert_eq!(
            send_retry_warning("!room:example.test", "not in room"),
            "sendAsAgent failed in room !room:example.test (after auto-join retry): not in room"
        );
    }
}
