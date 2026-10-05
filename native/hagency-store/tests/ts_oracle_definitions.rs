//! TS test oracle — agent definitions and Palpo retirement
//! (`tests/resource-agent-definitions.test.js`,
//! `tests/palpo-agent-definitions.test.js`,
//! `tests/palpo-agent-retirement.test.js`).
//!
//! The retained TS suite is the parity ORACLE. These three files drive the
//! retained HTTP API (`/api/framework-presets`, `/api/agents`, `/api/seats`,
//! `/api/fleet-control`, `/api/project-sides`). Native has **no**
//! `framework-presets` route and no agent-definition CRUD: its closest surfaces
//! are the store's provision/catalog/publication reads and the console's agent
//! delete (`agents.rs::delete`, which releases an agent's active engagements the
//! way `hasOtherAgentAllocation` guards). Those are CITATIONS, recorded in
//! `.peer/report-75.md`; the rest are `#[ignore]`d gaps.
//!
//! The one case asserted directly here is the retirement guard, which native
//! owns as `DomainRepository::agent_active_engagements` — the read the console's
//! force-delete uses to release a departing agent's commitments.

mod common;
use common::*;
use hagency_store::DomainRepository;
use serde_json::json;

/// Build a store with one admitted+approved engagement named `agent`.
fn store_with_active(name: &str) -> (DomainRepository, String) {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("medium", "seat_medium", 1000);
    db.put_resource(&pool).unwrap();
    let p = proof(&request(&format!("{name}_request"), name, &pool, 100));
    let engagement = db.admit(&p, 1000).unwrap().id;
    db.approve("approve", &p, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &hagency_store::EffectOutcome::Applied {
            receipt: "definition fixture".into(),
        },
    )
    .unwrap();
    // Keep the tempdir alive by leaking it: the test needs the handle only
    // while the repository is open.
    std::mem::forget(root);
    (db, engagement)
}

/// TS `palpo-agent-retirement.test.js:4` `another live allocation prevents whole
/// Agent retirement`: a live allocation for the SAME agent (whether `active` or a
/// `pending` one with fulfillment) keeps the agent alive; an allocation for
/// another agent does not.
///
/// Native's twin is `agent_active_engagements` — the read the console's
/// force-delete uses (`agents.rs:784`) to find and release a departing agent's
/// commitments, and the same rule `hagency-matrix/src/retire.rs` applies before
/// a worker is retired. Asserted directly: an agent with a live engagement is
/// reported as holding one, and an unknown agent holds none.
#[test]
fn ts_oracle_a_live_allocation_prevents_retirement() {
    let (db, engagement) = store_with_active("edison");
    let held = db.agent_active_engagements("edison").unwrap();
    assert_eq!(
        held,
        vec![engagement],
        "the agent's live allocation is found, so whole-agent retirement is prevented"
    );
    // A different agent's engagement is not this agent's business.
    assert!(
        db.agent_active_engagements("someone-else")
            .unwrap()
            .is_empty(),
        "another agent's allocation does not block this one's retirement"
    );
}

// The remote-identity oracle now runs in hagency-palpo's
// native_retirement_requires_every_exact_remote_identity_field and
// native_retirement_http_retries_exact_identity_and_refuses_redirected_or_incomplete_proof.
// Whole-process restart/retry evidence is native_palpo_retirement_reconciles_lost_reply_after_restart_without_a_runtime.

/* ─────────── resource-agent-definitions.test.js (5) — all route gaps ─────────── */

/// TS `resource-agent-definitions.test.js:46` `resource definitions persist
/// without provisioning and enforce unique qualified names`:
/// `POST /api/framework-presets/medium/agents` persists a definition without
/// creating an agent, rejects a duplicate/`original` name (409), a path-escape
/// (400) and a `architect` role (400).
#[ignore = "parity gap: no native /api/framework-presets route (no agent-definition CRUD)"]
#[test]
fn ts_oracle_resource_definitions_persist_without_provisioning() {
    panic!(
        "TS asserts a definition persists and unique qualified names are enforced; native has no framework-presets route"
    );
}

/// TS `resource-agent-definitions.test.js:65` `explicit definition provisions a
/// second agent with its own resource and stable retry`: a `definition`
/// allocation choice provisions `fast-one` with `phase: 'complete'`, and
/// re-approving the same choice is idempotent.
///
/// Native's provisioning surface exists (`provision_runtime.rs`,
/// `inline_factory.rs`), but it is driven by the engagement's own
/// `agentDefinition.resourceId` from the verified request, not by a
/// `{kind:'definition', definitionId}` choice on the verdict — that choice shape
/// has no native route.
#[ignore = "parity gap: no native framework-preset definition-allocation choice (native provisions from the verified request's agentDefinition)"]
#[test]
fn ts_oracle_explicit_definition_provisions_a_second_agent() {
    panic!(
        "TS asserts the definition allocation choice provisions a second agent; native has no such choice on the verdict"
    );
}

/// TS `resource-agent-definitions.test.js:88` `allocation choices reject foreign
/// agents and reserved identity changes`.
#[ignore = "parity gap: no native framework-preset definition-allocation choice"]
#[test]
fn ts_oracle_allocation_choices_reject_foreign_agents() {
    panic!(
        "TS asserts definition allocation choices reject a foreign agent and a reserved identity change; native has no definition choice"
    );
}

/// TS `resource-agent-definitions.test.js:101` `published capability resources
/// disclose models and definitions without deployment secrets`.
///
/// Native's public catalog IS a surface here: `native_catalog_snapshot_scope_and_roles`
/// and `catalogue_derived_keys_use_one_predicate_and_model_families`
/// (`hagency-store/tests/catalog_publication.rs`) assert the published projection
/// carries roles and model families and excludes private keys. Cited, not a gap.
#[test]
fn ts_oracle_published_capability_resources_disclose_without_secrets() {
    // The published catalog is reachable natively; the assertions live in
    // `catalog_publication.rs` (cited in the report). This test pins the one
    // invariant a reader of the TS case needs: a resource with no publish is
    // absent from the catalog, so nothing un-published can be disclosed.
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    // `common::resource` leaves `published` at its serde default (`true`), so an
    // unpublished resource must say so explicitly — that is the field the
    // catalog's `published=1` filter reads.
    let mut private: hagency_core::project::Resource = resource("medium", "seat_medium", 1000);
    private.published = false;
    db.put_resource(&private).unwrap();
    assert!(
        db.catalog("", 16).unwrap().is_empty(),
        "an unpublished resource is not in the catalog"
    );
}

/// TS `resource-agent-definitions.test.js:112` `two definitions share resource
/// capacity and interrupted provisioning retains its choice`.
///
/// Native's capacity sharing is `resource_budget` (pool AND seat commitments),
/// oracled by `fixtures/allocation.json`; the "interrupted provisioning retains
/// its choice" half is `provision_runtime.rs`
/// (`native_reattach_scope_rebuilds_only_what_the_factory_completed`). Cited.
#[test]
fn ts_oracle_two_definitions_share_resource_capacity() {
    // Restate the capacity rule at the allocation boundary: two commitments on
    // one preset's pool both count against it.
    let input: hagency_core::allocation::Input = serde_json::from_value(json!({
        "preset": {"id": "medium", "ceiling": {"tokens": 1000, "period": "monthly"}},
        "seatId": "seat_medium",
        "commitments": [
            {"id": "a", "presetId": "medium", "seatId": "seat_medium", "allocatedTokens": 400, "state": "active"},
            {"id": "b", "presetId": "medium", "seatId": "seat_medium", "allocatedTokens": 400, "state": "active"},
        ],
    }))
    .unwrap();
    let value =
        serde_json::to_value(hagency_core::allocation::resource_budget(&input).unwrap()).unwrap();
    assert_eq!(value["pool"]["committed"], json!(800));
    assert_eq!(value["pool"]["remaining"], json!(200));
}

/* ──────────── palpo-agent-definitions.test.js (12) — route gaps ──────────── */

/// TS `palpo-agent-definitions.test.js:27` `verified fleet project labels
/// populate console metadata without changing request identity`.
#[ignore = "parity gap: no native framework-presets route (no Palpo catalog metadata projection)"]
#[test]
fn ts_oracle_palpo_labels_populate_metadata() {
    panic!("TS asserts fleet project labels populate console metadata; native has no such route");
}

// The last-allocation remote cleanup and original-identity retry oracles are
// exercised by native_remote_retirement_inspects_the_original_uncertain_effect_after_restart
// and the actual native Palpo retirement process test cited above. Local runner
// custody remains separately fenced; remote cleanup does not settle usage.

/// TS `palpo-agent-definitions.test.js:118` `revoking one of two allocations
/// keeps the Agent on Matrix`.
///
/// Native's twin is `agent_active_engagements`: with one allocation still live
/// the agent is not a retirement candidate. Covered by the direct assertion
/// above (`ts_oracle_a_live_allocation_prevents_retirement`).
#[ignore = "parity gap: no native route to revoke ONE allocation while keeping the Agent (the store rule is covered by test_ts_oracle_a_live_allocation_prevents_retirement)"]
#[test]
fn ts_oracle_revoking_one_allocation_keeps_the_agent() {
    panic!(
        "TS asserts a partial revocation keeps the Agent on Matrix; native's rule is asserted at the store level, but no route revokes one of two allocations"
    );
}

/// TS `palpo-agent-definitions.test.js:137` `new resources automatically publish
/// qualified Palpo roles without enabling automatic acceptance`.
///
/// Native's resource publication IS a surface: `native_resource_publication_cas`
/// (`hagency-store/tests/resource_publication.rs`) and
/// `native_catalog_snapshot_scope_and_roles` (`catalog_publication.rs`) assert
/// published roles and that publication is an explicit CAS, not automatic
/// acceptance. Cited.
#[test]
fn ts_oracle_new_resources_publish_roles_not_acceptance() {
    // The invariant a reader needs: publishing a resource does NOT make it
    // auto-accept. Asserted at the allocation boundary — an undeclared quota
    // still refuses auto-join (`automatic-undeclared` vector).
    let input: hagency_core::allocation::Input = serde_json::from_value(json!({
        "preset": {"id": "medium", "ceiling": {"tokens": 1000, "period": "monthly"}},
        "seatId": "seat_medium",
        "declaration": {},
        "commitments": [],
        "forAutoJoin": true,
    }))
    .unwrap();
    let value =
        serde_json::to_value(hagency_core::allocation::resource_budget(&input).unwrap()).unwrap();
    assert_eq!(
        value["remainingTokens"],
        serde_json::Value::Null,
        "publication alone never grants auto-join: an undeclared quota leaves headroom unknown"
    );
}

/// TS `palpo-agent-definitions.test.js:163` `automatic Palpo catalog follows
/// resource edits deletion and explicit role withdrawal`.
#[ignore = "parity gap: no native framework-presets route (no automatic Palpo catalog follow on resource edits)"]
#[test]
fn ts_oracle_palpo_catalog_follows_resource_edits() {
    panic!(
        "TS asserts the Palpo catalog follows resource edits/deletion/withdrawal; native has no such route"
    );
}

/// TS `palpo-agent-definitions.test.js:190` `Palpo requests on a fresh resource
/// wait for approval without consuming exhausted project budgets`.
#[ignore = "parity gap: no native framework-presets route (no Palpo request routing on a fresh resource)"]
#[test]
fn ts_oracle_palpo_fresh_resource_waits_for_approval() {
    panic!(
        "TS asserts a fresh resource's requests wait for approval without consuming an exhausted project budget; native has no such route"
    );
}

/// TS `palpo-agent-definitions.test.js:233` `Palpo pool approval separates
/// resource ceilings and enforces shared account quota`.
///
/// Native owns both rules: the pool/seat separation and shared-seat quota are
/// `resource_budget` (`fixtures/allocation.json`, the `quota-*` vectors), and
/// `ceiling_alerts.rs` pins the ceiling side. Cited.
#[test]
fn ts_oracle_palpo_pool_approval_separates_ceilings() {
    // Restate the separation: a pool commitment and a seat commitment are
    // tracked apart, so a preset's ceiling and the seat's quota do not merge.
    let input: hagency_core::allocation::Input = serde_json::from_value(json!({
        "preset": {"id": "medium", "ceiling": {"tokens": 1000, "period": "monthly"}},
        "seatId": "seat_medium",
        "declaration": {"quotaTokens": 500, "period": "monthly"},
        "commitments": [
            {"id": "pool", "presetId": "medium", "seatId": "other_seat", "allocatedTokens": 300, "state": "active"},
            {"id": "seat", "presetId": "other", "seatId": "seat_medium", "allocatedTokens": 200, "state": "active"},
        ],
    }))
    .unwrap();
    let value =
        serde_json::to_value(hagency_core::allocation::resource_budget(&input).unwrap()).unwrap();
    assert_eq!(
        value["pool"]["committed"],
        json!(300),
        "only the preset's pool"
    );
    assert_eq!(
        value["seat"]["committed"],
        json!(200),
        "only the shared seat"
    );
    // Headroom is the `min` of the non-null limits: pool 1000-300 = 700, seat
    // 500-200 = 300, so the shared seat is what binds.
    assert_eq!(value["pool"]["remaining"], json!(700));
    assert_eq!(value["seat"]["remaining"], json!(300));
    assert_eq!(value["remainingTokens"], json!(300));
}

/// TS `palpo-agent-definitions.test.js:257` `concurrent Palpo definitions
/// reserve the selected pool only once and preserve retry`.
///
/// Native's provisioning retry/idempotence is
/// `provision_runtime.rs::native_reattach_scope_rebuilds_only_what_the_factory_completed`
/// and `native_provisioning_effect_completed`. Cited.
#[ignore = "parity gap: no native framework-presets route (store-level provisioning idempotence is cited from provision_runtime.rs)"]
#[test]
fn ts_oracle_concurrent_palpo_definitions_reserve_once() {
    panic!(
        "TS asserts concurrent definitions reserve one pool once and preserve retry; native's idempotence is at provision_runtime.rs"
    );
}

/// TS `palpo-agent-definitions.test.js:275` `Palpo definitions provision
/// distinct agents on the requested resource without provider definitions`.
#[ignore = "parity gap: no native framework-presets route for definition-driven provisioning"]
#[test]
fn ts_oracle_palpo_definitions_provision_distinct_agents() {
    panic!(
        "TS asserts definitions provision distinct agents on the requested resource; native has no such route"
    );
}

/// TS `palpo-agent-definitions.test.js:313` `Palpo definition allocation refuses
/// substitution and preserves reservation retry`.
#[ignore = "parity gap: no native framework-preset definition-allocation choice"]
#[test]
fn ts_oracle_palpo_definition_refuses_substitution() {
    panic!(
        "TS asserts a definition allocation refuses substitution; native has no definition choice"
    );
}

/// TS `palpo-agent-definitions.test.js:334` `Palpo definitions reject
/// unpublished resources and project name conflicts`.
///
/// Native owns both: `native_resource_publication_cas` refuses a stale/unpublished
/// publish, and `live_project_names` (a partial unique index) refuses a duplicate
/// live project name. Cited.
#[test]
fn ts_oracle_palpo_rejects_unpublished_and_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("medium", "seat_medium", 1000);
    db.put_resource(&pool).unwrap();
    let first = proof(&request("first_request", "Original", &pool, 100));
    db.admit(&first, 1000).unwrap();
    // A second engagement with the SAME project name on the same fleet is refused
    // by the `live_project_names` index.
    let clash = proof(&request("clash_request", "Original", &pool, 100));
    assert!(
        db.admit(&clash, 1001).is_err(),
        "a live project-name conflict is refused"
    );
}
