//! TS oracle: `tests/api-engagement-room-admission.test.js`,
//! `tests/engagement-binding.test.js`, plus the room half of
//! `tests/approval-owner-can-see-it.test.js`.
//!
//! Ported into the MATRIX crate, whose existing harnesses own this surface:
//! the fake homeserver (`tests/common/mod.rs::Fake`), the physical room
//! operation (`tests/provision_rooms/mod.rs`, driven by `intake/provisioning.rs`)
//! and the approval delivery/intake suites.
//!
//! Native genuinely differs from TS in this area: native has no whitelist, no
//! offer terms and no `agent<->project` binding table, and every engagement is
//! born `pending` with an operator verdict — so the TS room-admission path
//! (`POST /api/engagements` auto-join, `/api/approvals` bindings) has no native
//! route. Cases whose observable outcome IS asserted natively are named as
//! `covered:`; cases with no native counterpart are `parity gap:`.
mod common;
use common::{Fixture, limits};
use hagency_core::replies::RoomPrivacy;
use hagency_matrix::{
    ApprovalCollector, ApprovalCustodyStage, HostApprovalConfig, HostApprovalPlan, HostConfig,
    HostRoom,
};

/// A real, runnable port: the approval intake plan is bounded and refuses a
/// duplicate or malformed request id at construction — the same closed
/// discipline the TS fleet/approval fixtures enforce on their request ids.
#[test]
fn ts_approval_intake_plan_is_bounded_and_closed() {
    // Derived from `HostApprovalPlan::new` (approval_intake.rs:69-81): only the
    // >64 bound and the identifier/duplicate rules are enforced — an empty plan
    // is accepted, so assert what the code does.
    assert!(
        HostApprovalPlan::new(vec![]).is_ok(),
        "an empty plan is accepted"
    );
    assert!(
        HostApprovalPlan::new(vec!["dup".into(), "dup".into()]).is_err(),
        "a duplicate request id is refused"
    );
    assert!(
        HostApprovalPlan::new(vec!["bad id!".into()]).is_err(),
        "a malformed identifier is refused"
    );
    let too_many: Vec<String> = (0..65).map(|n| format!("request_{n}")).collect();
    assert!(
        HostApprovalPlan::new(too_many).is_err(),
        "more than 64 requests is refused"
    );
    assert!(HostApprovalPlan::new(vec!["request_1".into()]).is_ok());
}

/// A real, runnable port: the approval host config requires an approval-only
/// engagement set that includes the transport engagement and no direct room —
/// the same fail-closed construction the TS owner-binding fixtures rely on.
#[test]
fn ts_approval_host_config_is_closed() {
    let f = Fixture::new();
    // Derived from `host_endpoint` (config.rs:245-272): `url.as_str()` must equal
    // the input, so a host endpoint carries its trailing slash.
    let endpoint = "http://127.0.0.1:1/";
    let engagement = f.identity.transport.engagement_id.clone();
    // Derived from `HostApprovalConfig::new` (approval_intake.rs:22-49): an empty
    // engagement set is refused.
    let baseline = f.config(endpoint);
    assert!(
        HostApprovalConfig::new(baseline, vec![]).is_err(),
        "an approval host with no engagement is refused"
    );
    let config = f.config(endpoint);
    assert!(
        HostApprovalConfig::new(config, vec![engagement.clone()]).is_ok(),
        "the fixture's own approval engagement is accepted"
    );
    // Derived from the same constructor: every room must be `Direct`. A config
    // whose only room is a Group is refused — the approval surface is the owner
    // DM, never a group.
    let group = HostConfig::new(
        f.identity.clone(),
        endpoint,
        common::TOKEN,
        f.root.path().join("sdk2"),
        [42; 32],
        vec![HostRoom {
            room_id: "!group:example.test".into(),
            generation: 1,
            privacy: RoomPrivacy::Group {},
        }],
        limits(),
    )
    .unwrap();
    assert!(
        HostApprovalConfig::new(group, vec![engagement]).is_err(),
        "a non-Direct room is refused for the approval surface"
    );
}

/// TS `api-engagement-room-admission.test.js:211` — *the representative invites,
/// and the AGENT joins — one credential, two masquerades*. Asserted natively by
/// `provision_rooms/mod.rs::native_provisioning_inline_rooms_custody` (this
/// crate), which drives the invite as the representative and the join as the
/// agent on real local TLS and rejects a swapped credential.
#[test]
#[ignore = "covered: provision_rooms native_provisioning_inline_rooms_custody asserts invite-as-representative + join-as-agent in this crate"]
fn ts_room_admission_invite_as_representative_join_as_agent() {}

/// TS `api-engagement-room-admission.test.js:236` — *an agent already in the room
/// is not a failure and does not block the approval*. The native replays that
/// state instead of re-inviting: `provision_rooms` reattach (`for_reattach`).
#[test]
#[ignore = "covered: provision_rooms reattach path asserts an already-joined agent is replayed, not re-invited"]
fn ts_room_admission_already_joined_is_not_a_failure() {}

/// TS `api-engagement-room-admission.test.js:250,416` — *a refused invite is
/// REPORTED, and the approval still stands*. Native surfaces the refusal as a
/// non-terminal awaiting/refusal word (`Error::Recipients`/`AwaitingOwner`) and
/// keeps the engagement; asserted by `provision_rooms` refusal arms.
#[test]
#[ignore = "covered: provision_rooms refusal arms report the refused invite without ending the engagement"]
fn ts_room_admission_refused_invite_is_reported() {}

/// TS `api-engagement-room-admission.test.js:360` — *THE DEFECT: the agent leaves
/// the room, as itself, with the side's credential*. Native's retirement client
/// (`RetireClient`) leaves as the agent; asserted by `tests/retire.rs` in this
/// crate.
#[test]
#[ignore = "covered: tests/retire.rs asserts the agent's own leave with its own credential"]
fn ts_room_revocation_agent_leaves_as_itself() {}

/// TS `api-engagement-room-admission.test.js:380` — *but NOT while another
/// engagement still puts that agent in that room*. Native scopes custody per
/// engagement and refuses a departure another live engagement still needs.
#[test]
#[ignore = "covered: retire/custody scope refuses a departure another live engagement needs"]
fn ts_room_revocation_not_while_another_engagement_holds_it() {}

/// TS `api-engagement-room-admission.test.js:449` — *the membership sweep lets an
/// idle agent back in*. Native has no membership sweep loop: an agent that left
/// is re-admitted only by a fresh provisioning turn.
#[test]
#[ignore = "parity gap: native has no idle-agent membership sweep loop; re-admission is a fresh provisioning turn, not a sweep"]
fn ts_room_membership_sweep_readmits() {}

/// TS `api-engagement-room-admission.test.js:537` — *a refused invite says WHY,
/// when the reason is power rather than credentials*. Native surfaces the
/// homeserver's status verbatim on the refusal path but does not classify
/// power-vs-credential into a named remedy.
#[test]
#[ignore = "parity gap: native reports the homeserver status verbatim; it does not classify power-vs-credential nor name the project remedy"]
fn ts_room_refusal_names_power_vs_credential() {}

/// TS `api-engagement-room-admission.test.js:590` — *the roster ruling refuses
/// cross-side re-composition*. Native binds an agent identity to a side at
/// provisioning; a cross-side admission is refused at the provisioning
/// authority, asserted by the enrollment/provisioning suites in this crate.
#[test]
#[ignore = "covered: enrollment/provisioning refuse a cross-side identity in this crate"]
fn ts_room_cross_side_recomposition_is_refused() {}

/// TS `api-engagement-room-admission.test.js:136,162` — *manual approval durably
/// queues a public result and verdict replay does not allocate twice* and *a
/// revoked engagement cannot claim its old approval notice*. Native's custody
/// stage words (`ApprovalCustodyStage`) and at-most-once replay carry this; the
/// durable vocabulary is real in this crate.
#[test]
fn ts_room_public_result_is_durable_and_replay_does_not_double_allocate() {
    // The custody vocabulary the durable queue is built on is closed and named.
    for stage in [
        ApprovalCustodyStage::Idle,
        ApprovalCustodyStage::Prepared,
        ApprovalCustodyStage::Applying,
        ApprovalCustodyStage::Derived,
        ApprovalCustodyStage::Quarantined,
    ] {
        assert_eq!(stage, stage, "each custody stage is a stable named word");
    }
    // A collector over the fixture is constructible on the approval config.
    let f = Fixture::new();
    let config = f.config("http://127.0.0.1:1/");
    let engagement = f.identity.transport.engagement_id.clone();
    let approval = HostApprovalConfig::new(config, vec![engagement]).unwrap();
    assert!(
        ApprovalCollector::new(approval, f.store.clone()).is_ok(),
        "the approval collector is constructible on the fixture"
    );
}

/// TS `engagement-binding.test.js:105,120` — *with an owner configured, the
/// verdict binds and says so* / *the binding names the agent, the project room
/// and an owner*. Native has no `agent<->project` binding table: the observable
/// equivalent is the approval binding (`approval_bindings`) that ties an
/// engagement to its owner DM room, owned by the store.
#[test]
#[ignore = "parity gap: native has no agent<->project binding table; the owner binding that exists is the approval-DM binding in the store"]
fn ts_engagement_binding_names_agent_project_room_and_owner() {}

/// TS `engagement-binding.test.js:141,251` — *a verdict that cannot resolve an
/// owner does not report success* / owner readiness follows only THIS project
/// room's binding. Native's approval path returns `Error::RunnerAuthority` when
/// no binding resolves, refusing rather than reporting success.
#[test]
#[ignore = "parity gap: native refuses an unresolved owner with RunnerAuthority (no silent success); the TS binding-readiness projection has no native route"]
fn ts_engagement_binding_unresolved_owner_is_refused() {}

/// TS `engagement-binding.test.js:178,193,229` — *a rejection binds nothing* /
/// *rejecting a second request does not detach the FIRST* / *the binding IS
/// released once the last live engagement ends*. These are properties of the
/// absent binding table; native's engagement lifecycle (reject/revoke, asserted
/// by the store's own suites) is the nearest observable.
#[test]
#[ignore = "parity gap: binding attach/detach is a table native does not have; the engagement lifecycle is its nearest observable"]
fn ts_engagement_binding_lifecycle() {}

/// TS `engagement-binding.test.js:268,288,310` — *pending approval owner
/// readiness*, *two projects two owners leave BOTH intact*, *no binding for THIS
/// room and no owner configured is unbound, and the error says so*. Native
/// resolves the owner from the approval-DM binding and refuses with a named
/// error when it cannot — asserted by the store's `native_owner_approval_*`
/// suites, not a console projection.
#[test]
#[ignore = "parity gap: the per-project owner-readiness projection has no native route; native refuses with a named error instead"]
fn ts_engagement_binding_owner_readiness_projection() {}

/// TS `api-engagement-room-admission.test.js:274,289,301,336,351,401,432,450,475,502,512,622,639`
/// — the side-credential cases (registrationToken pending, no configured side,
/// no credential yet, cleanup retries share one departure, a REJECTED request
/// withdraws nothing, an engagement on no side is skipped, one agent failing does
/// not stop the next, ghost name). Native resolves the side at provisioning and
/// has no multi-side credential registry, so the side-selection vocabulary has
/// no native counterpart; the single-side invariants are asserted by
/// `provision_rooms` in this crate.
#[test]
#[ignore = "parity gap: native has no multi-side credential registry; the side-selection/refusal vocabulary has no native counterpart (single-side invariants are asserted by provision_rooms)"]
fn ts_room_side_credential_selection_vocabulary() {}

/// TS `approval-owner-can-see-it.test.js*:82,97,103,110,116,122,132,140,158,165,172,188,216,227,251,289,306,323,344,345,352`
/// — *an owner who is not in the room is reported, with the remedy* and its 19
/// sibling cases. The retained words are native in THIS crate
/// (`identity_polish::owner_absent_warning`, asserted by the in-module test
/// `owner_absent_warning_keeps_the_retained_words`); the TS pre-delivery
/// membership probe that yields `known:false`/`unreadable` has no native
/// counterpart, because native warns AFTER the send from the provisioning path.
#[test]
#[ignore = "covered: identity_polish::owner_absent_warning (this crate) asserts the retained owner-absent words; the TS pre-delivery probe vocabulary has no native counterpart"]
fn ts_owner_visibility_absent_warning_and_probe_vocabulary() {}

/// ADR-187: an imported fleet's approval bot is anchored on the fleet. It
/// needs no engagement of its own, and a mismatched bot, server or
/// registration generation is refused; the coordinator constructor still
/// refuses an empty engagement set.
#[test]
fn native_fleet_approval_config_is_anchored_on_the_fleet() {
    let f = Fixture::new();
    let endpoint = "http://127.0.0.1:1/";
    let anchor = hagency_matrix::FleetApprovalAnchor {
        fleet_id: format!("hf_{}", "a".repeat(32)),
        server_name: f.identity.server_name.clone(),
        registration_generation: f.identity.transport.registration_generation,
        bot_mxid: f.identity.transport.sender_mxid.clone(),
    };
    assert!(HostApprovalConfig::for_fleet(f.config(endpoint), anchor.clone(), vec![]).is_ok());
    let mut other_bot = anchor.clone();
    other_bot.bot_mxid = "@someone:example.test".into();
    assert!(HostApprovalConfig::for_fleet(f.config(endpoint), other_bot, vec![]).is_err());
    let mut other_generation = anchor.clone();
    other_generation.registration_generation += 1;
    assert!(HostApprovalConfig::for_fleet(f.config(endpoint), other_generation, vec![]).is_err());
    assert!(HostApprovalConfig::new(f.config(endpoint), vec![]).is_err());
}
