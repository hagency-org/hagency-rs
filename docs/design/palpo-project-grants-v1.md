# Palpo project grants: reservation contract

Status: local grant/accounting and authenticated command/receipt implementation
for Rinx ADR 0010. This is not an advertised workflow capability or a deployed
workflow. The operator contribution UI and bounded snapshots are implemented;
Palpo Inbox commands and Rinx forms still need integration before enabling the capability.

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
authenticated identity. The consumer obtains registration from its existing authenticated outbound
session. Palpo must derive the decision actor from the current Matrix/app
session before enqueueing; Hagency also obtains a fresh authorization lease
from the fixed Palpo machine endpoint immediately before execution.

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
approved. Explicit migration and refunds for any grant that funded an agent
require additional reviewed operations; this foundation does not manufacture either.

An explicit `release_unused_project` command can return a failed project's whole
reservation to its contribution, under the designated Palpo administrator's fresh
authorization. The writer verifies the current registration, exact grant revision
and absence of any lifetime `project_grant_agents` debit. It sets revocation and
the schema-64 `released_at` marker in the receipt transaction. The original grant
row remains permanently, fencing delayed admission, reused grant IDs and legacy
console approval. Expiry alone is not release. A grant that ever funded an agent
is refused even after that agent ends or its ordinary history is retained away.
Same-ID/restart retries return the original receipt; another project can use the
returned contribution capacity, while the provider's contribution stays reserved.

## Command and receipt transport

`ProjectCommand` v1 uses the existing authenticated work lane, kind `workflow`.
It binds command ID, exact argument digest, fleet/registration generation,
issuer, actor, deadline and a closed operation: reserve project, replace explicit
administrators, approve/reject agent, top up agent, revoke agent/project, and
release a provably unused project reservation. Unknown
fields are rejected, including fields nested inside legacy request DTOs. There
is no arbitrary URL, script, console command or credential in the payload.
The shared Rust/Palpo corpus is `native/fixtures/project-commands.json` (copied
unchanged to Palpo `web-admin/test/fixtures/project-commands.json`).

Palpo's enqueue API requires the caller's existing SQLite decision transaction;
Inbox integration must use this API for each human decision. An outbound
transport acknowledgement proves custody only. Hagency commits the
business operation and immutable typed receipt together (schema 62). Fresh agent
admission shares this transaction; a failed decision/receipt insert cannot leave
an orphan pending agent. Definitive scope, capacity, expiry and state refusals
produce receipts. Database/authority availability and expired authorization
leases remain retryable. An expired command is refused locally even if Palpo or
Matrix is unavailable. Receipt replay never repeats a debit or resurrects a
later revoked agent.

Before execution, Hagency POSTs command ID/digest to its fixed authenticated
`authorize-command` endpoint. Palpo rechecks the actor's current Matrix account,
the designated project approver or explicit assigned project administrators,
self-approval policy and current/pending grant revision. An unavailable Matrix
lookup returns 503, not a permanent denial. The resulting lease lasts ten
seconds; Hagency checks it again after the actual SQLite writer lock. Slow
Matrix request verification is followed by another authorization check. This
bounds the distributed authorization window; it does not claim instantaneous
revocation across a lease already issued by another service.

Up to eight pending business receipts join a frozen outbound update. A lost
response/restart repeats those original bytes and sequence. Only the exact
receipts included in the acknowledged update are marked published. Palpo binds
results to original commands and persists receipts, status and transport sequence
atomically. Repeated historical receipts do not restore old administrator lists.

Status observations use bounded pages scoped to the current fleet registration,
including agents known through durable workflow receipts. A producer advances
after its exact frozen page is acknowledged; agents beyond the first 100 are
not permanently omitted. Matrix room membership is still observed before Ready.

## Operator contributions and publication

Hagency's My resources page offers contribution creation and revocation for an
existing, reception-bound Palpo registration. The original authenticated console
session gates each queued mutation. The writer checks resource revision,
registration and provider account before committing through the existing
reservation kernel. The UI collects finite tokens, agent count, aggregate daily
rate and expiry. It saves the exact request before sending; response loss and
reload retry the same ID and body. Revocation leaves its budget held.

`contributionPage` on authenticated outbound updates contains at most 16 rows,
registration generation, observation time and a cursor. Each row includes its
immutable grant, state and aggregate lifetime project reservations. Pages exclude
other fleets and prior registrations. Frozen updates retain their original page
and advance only after acknowledgement, including restart. An empty page is not
deletion. Publication does not advertise the complete project workflow capability.

Palpo commits validated pages with the transport sequence. Pages now include
cumulative `reserved` and `released` limits. Both are monotonic; held capacity is
their difference. Cumulative reservations may exceed the original budget after
reuse, but held capacity cannot, and all sums remain JSON-safe integers. Old
frozen pages without `released` deserialize as zero. The console displays the
held amount. Palpo refuses changed grant identities/budgets, decreasing counters,
restoration after retirement,
foreign fleets and old registration generations. Accepted project receipts now
retain their registration generation so a new registration cannot authorize work
against an old grant. UI availability still needs to check freshness, current
generation and remaining limits; a cached observation is not an allocation.
The release operation additionally needs `projectWorkflow.unusedRelease: true`.
Hagency still withholds the complete workflow capability pending integration
acceptance; the local fixture explicitly advertises the extension for tests.

## Integration still required

Hagency-originated association still needs its complete review/handoff flow.
Palpo's project review UI
must collect finite budgets and explicit administrators, create the actual owner
room, enqueue reservation and show Awaiting reservation until its applied
receipt. Assigned-admin Inbox decisions then enqueue agent/top-up/revoke work.
Ready requires runtime and actual Matrix membership evidence. Usage freshness,
cleanup results, owner removal and notification projection still need their full
cross-service integration. Existing project owners remain unchanged.

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
