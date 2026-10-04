# Palpo project grants: reservation contract

Status: local implementation of the grant and accounting foundation for Rinx
ADR 0010. This is not an enabled wire capability or a deployed workflow.
The operator UI, authenticated command delivery, publication receipts and Palpo
Inbox integration are subsequent work. Do not advertise support until those
paths and their recovery tests exist.

## Authority

Hagency owns resources. Its authenticated operator explicitly contributes a
finite budget to one current Palpo fleet registration. Publishing a catalog item,
verifying the connection, Matrix room power, or logging into Rinx does not create
this authority.

The designated Palpo administrator approves project creation and assigns project
administrators. The project owner remains the requesting manager. The assigned
administrators approve agent allocations and top-ups inside the accepted project
budget. There is no second human Hagency verdict. Execution/tool approvals still
belong to the existing owner-authorized protocol.

The Rust domain interfaces are trusted host interfaces. A `Registration` value
or `ProjectAgentDecision` deserialized from an arbitrary HTTP body is not an
authenticated identity. The future consumer must obtain registration from its
existing authenticated outbound session, and Palpo must derive the decision actor
from the current Matrix/app session and recheck role scope before enqueueing.

## Stored contract

`ResourceDelegation` version 1 binds ID/revision, fleet and registration
generation, issuer server name, resource ID, positive token/agent/rate limits and
an absolute expiry. The current registration must match. Omitted limits never
mean unlimited. All integer quantities are bounded to JavaScript-safe precision;
agent count is at most 10,000.

`ProjectGrant` version 1 binds the contribution ID/revision, project ID, actual
project room, owner Matrix ID, explicit administrator Matrix IDs, explicit
self-approval policy, budget and expiry. Project users and room must belong to
the contribution's issuer. The project expires no later than its contribution.
One fleet/project/resource combination has one stored grant. Creation retries
with the same ID and content reuse it; changed content conflicts.

Administrator reassignment increments the project revision without moving its
owner, room or budget. Old decisions cannot use a newer assignment implicitly.
A repeat of the exact reassignment can recover its accepted revision. Project
and contribution revocation use expected revisions.

## Accounting and recovery

One immediate SQLite transaction reserves provider capacity, or assigns an
agent/top-up from a previously accepted project grant. Aggregate project limits
fit their contribution. Agent token debits are lifetime allocations; active
agent slots and declared daily rates fit the project's aggregate limits.

Provider reservation is counted once. Existing engagement allocations remain in
the normal resource/seat accounting, while the unassigned portion of a
contribution is added to it. Two resource presets sharing a provider seat cannot
reserve the same capacity. A later lower resource/seat policy can refuse new
allocations even when a project has unspent budget.

An agent approval commits its budget debit, engagement, provision effect and
command receipt together. A top-up commits its debit, increase to the same
engagement and quota-hold update together. Financial receipts survive ordinary
rolling audit-history retention. Reusing a command ID with changed content is a
conflict. Neither an agent's deletion nor retention is evidence of unused tokens.

Expired or revoked authority prevents new decisions and late provisioning
completion. Reconciliation uses the existing retirement transition: cancel and
fence provision effects, retire any possibly started physical agent, and revoke
execution authority. Cleanup can continue after grant expiry. Pending, uncertain
or failed cleanup never silently returns reserved capacity. Existing runner
capabilities cannot resume or submit accepted completion after revocation.

The owning worker reads the execution clock after acquiring SQLite's write lock
for reservation, approval, top-up and administrator reassignment. Caller timestamps
cannot preserve an approval's pre-queue expiry check. Direct repository methods
with explicit clocks remain available for deterministic domain tests.

Once a project has a grant, its new agent approvals must use the scoped project
decision path. The legacy console approval path cannot bypass it. Existing
engagements are not implicitly assigned to grants, renamed, transferred or
approved. Explicit migration and final capacity release require additional
reviewed operations; this foundation does not manufacture either.

## Transport work still required

Use finite versioned operations over the existing authenticated Palpo work lane:
reserve project, replace administrator assignment, decide agent, approve top-up,
revoke project/agent. Bind command ID, argument digest, current fleet generation,
actor, exact object/revision and deadline. No generic console operation or URL is
accepted. Persist a business receipt atomically with the operation, independently
of transport acknowledgement, and publish it with its original command digest.

Palpo commits its decision and outbound work together. It reports Awaiting
reservation until Hagency returns an accepted grant, and Provisioning until
runtime and actual Matrix room membership are observed. Expired, over-budget,
unknown and revoked outcomes must remain distinct. An old notification opens
current state; it is never a new decision command.

Publishing workflow support must also carry bounded contribution/grant snapshots,
current usage freshness and cleanup facts. The receiver must reject mismatched
receipts and preserve pending commands through lost responses and restart.
Demotion, grant revocation and generation rotation need cross-service race tests.

## Validation

`cargo test -p hagency-store --locked --test project_grants` exercises real SQLite
reservations, conflicting/replayed decisions, assignment and project scope,
explicit self-approval, concurrent allocation, provider-seat overlap, daily-rate
and slot aggregation, expiry, retirement races, stale runner capabilities and
post-lock expiry. Historical migration fixtures reconstruct their old schemas
before applying migration 61 (`075-project-grants.sql`).

These checks do not claim Makepad UI validation, deployment, mobile acceptance,
real provider token enforcement during an in-flight turn, final unused-capacity
refunds, or complete Palpo-to-Matrix agent lifecycle acceptance.
