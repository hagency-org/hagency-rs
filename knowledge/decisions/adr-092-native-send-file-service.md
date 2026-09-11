---
kind: decision
id: ADR-092
title: "Expose native scoped file delivery through the service and MCP"
status: Proposed
requirements: [REQ-RUST-MIGRATION-EXECUTION, REQ-THREAD-SCOPED-SESSIONS, REQ-MATRIX-DM-PRIVACY, REQ-THREE-LAYER-COMPLETION]
---

This is a review proposal, not an accepted implementation decision. The design
branch contains no production change or implemented acceptance selector. Root
review must accept the interfaces and ADR093 physical-root dependency before
implementation. ADR089 and ADR091 must be integrated first. The proposed task
contract is task-rust-native-send-file.spec.md; passing isolated primitives cannot
replace its executable service/MCP and encrypted-recipient acceptance.

## Observable result and initial scope

An already assigned native runner calls send_file with a stable call_id, a
workspace-relative path, optional recipient filename and optional caption. The
destination is its existing current encrypted conversation, including the original
thread relation when present. The host captures one original file, stages and
uploads that ciphertext once, then publishes one separately authorized encrypted
m.file event. A durable admission returns delivery_id and queued status promptly;
get_file_delivery returns delivered only after actual Matrix room-event acceptance.
Upload Accepted alone is not delivered. Neither operation changes canonical Done.

The first implementation supports regular files through m.file, configured existing
encrypted conversations and the actual configured byte limit. It does not implement
image previews, plaintext destinations, receive_file caches, new room discovery,
taskless uploads or automatic retries of unknown writes. The initial configured
limit is 4 MiB and may only be lowered in this slice. Native primitives have a
16 MiB hard maximum; legacy's 20 MiB advertisement must not be copied. Increasing
the supported product limit requires separate measured qualification.

## Executable ownership and configuration

The existing native serve command gains an explicit --file-service-config path.
Omission leaves file tools unavailable. Startup reads at most 16 KiB from an
already private, regular, non-symlink config file with a closed versioned schema.
No .env loading, environment credential discovery, arbitrary helper commands,
permission overrides or live-state import is introduced. Configuration is host
data, not execution authority, and has no Debug or log projection.

The bounded configuration selects one Matrix account/SDK owner, one original
registration/transport identity, at most 16 existing encrypted room bindings and
at most 16 workspace bindings. Matrix origin must satisfy existing HTTPS origin
validation; redirects and proxies remain disabled. Credentials and the SDK key
come from fixed private files under the configured fresh state root, read through
the existing private-file checks, rather than inline config values or CLI argv.
SDK and staging directories are fixed host-owned children of that state root.
Workspace selection names a host-provisioned ADR093 root, never a model-selected
ambient path. Unknown fields, duplicates, larger counts, invalid generations,
foreign roots or a conflicting existing SDK/state owner fail before admission.

The executable owns exactly one FileService, its Collector, physical workspace
registry and staging worker. App receives a shared FileService handle; MCP clients
use the same authenticated loopback API and never open the domain database. No
second writable domain owner, timer-based service discovery or hidden Node helper
is permitted. A startup error does not partially enable file routes. Reload or
replacement with outstanding jobs is refused; explicit SDK reopen preserves the
existing FileService and its retained jobs.

Configured file-service availability and agent_execution qualification are separate
capabilities. This slice cannot set agent_execution or production_api_parity true.
Only a qualified existing owned dispatch binding may use file operations. An
unavailable runtime remains unavailable even if a config names an executable.
The executable integration fixture may supply the existing fixed offline native
peer through test-only provisioning; no production flag authorizes an arbitrary
unqualified runtime. Both the normal startup wiring and actual MCP executable must
be exercised, not replaced by a fixture-only route or service implementation.

## Same physical root: required ADR093 interface

The file source and actual runner cwd must be bound to the same host-provisioned
workspace under a declared trust profile. FileService needs an actual retained
native root; Host.workspaces PathBuf, canonicalize, a root pathname or serialized
file identity alone cannot supply that file capability. ADR093 must deliver a
host-only shared binding usable by execution and hagency-files. FileService accepts
that binding and never opens a root from model request data.

The existing accepted baseline trusts the host to exclusively provision workspace
directories and their ancestors for the operation lifetime. private::check_handle
excludes malicious same-UID protection. Under that profile, the host must retain
the actual source root and maintain a stable launch pathname/ancestor binding;
configuration and dispatch IDs cannot substitute for either obligation. Source
selection still rejects traversal, symlink/reparse escapes, hard links and other
nonregular objects using actual relative handles. ADR093 must specify how the
same opened host root enters the file binding and remains associated with the
runtime launch, including detectable substitution refusal. No new hostile same-UID
isolation guarantee is inferred from the file-service requirement.

A stronger profile allowing hostile workspace/ancestor renames is separate. Linux
and macOS would need retained descriptor transfer through the actual guardian and
fchdir or another verified mechanism; Windows may need deny-delete/rename ancestor
handles and reparse refusal. Child cwd alone still does not qualify that stronger
profile: pinned Codex v0.153.4 canonicalizes cwd at Linux bwrap.rs:327/996 and
writable roots at :568-583 before its bind/chdir operations. A /proc descriptor
alias can therefore become an ambient pathname again. Stable namespaces or another
proven runtime-path solution are options for that stronger threat model, not a
universal prerequisite under the existing host-exclusive profile. Both the selected
profile and unsupported guarantees must be explicit; actual runtime sandbox and
hostile-child confidentiality remain separate qualification gates.

The execution owner registers a bounded opaque DispatchWorkspace only after the
actual Started acknowledgement, keyed to the complete original capability and
OwnedDispatchScope fingerprint. At most one active binding exists per configured
exclusive workspace, with at most 16 total. The service can only borrow that exact
binding. The domain writer validates current Started task/epoch, capability,
resource/lease and route before admission and again after the staging queue wait
immediately before capture. Binding retirement prevents new access; physical
custody needed by an already admitted snapshot/unknown operation remains held.
An operation registration with a lost Started acknowledgement never becomes a
file-access grant. Exact ADR093 type and method names are a review dependency,
not a license to introduce raw root handles over HTTP.

Executable driver integration is also an explicit review dependency. Current
serve has no owned-dispatch driver, and a reopened database cannot reconstruct
an original RunnerCapability. The implementation must connect the same-process
owned execution driver to this registry; neither config nor an HTTP authority
setter may synthesize a Started binding. Review must choose the bounded driver
entry and fixture provisioning before this proposal can become Accepted. The
required positive test may not silently substitute a test-only registered scope
for that missing production connection.

## Finite original capture and durable admission

FileService has two process-local retained job slots, shared by every App alias and
SDK reopen. A single dedicated owned worker thread owns the media Store and performs
blocking snapshot/encryption/staging work; its input queue holds only already
admitted slots and has capacity two. There is no extra waiting queue, thread per
HTTP request or unbounded spawn_blocking work. Two slots cover queued, executing,
unknown and uncommitted-response jobs. Large custody is released only on a known
terminal local refusal or complete historical settlement, never on caller drop.
The implementation must not block a Salvo worker while joining this thread.

Inputs are bounded before copies: call_id <=128 bytes, RelativeFile's existing
4096-byte/32-component syntax, filename <=255 bytes, caption <=1000 bytes and the
existing exact capability syntax. The complete encoded request is <=16 KiB.
The host derives the default filename from the relative selection and uses fixed
application/octet-stream metadata for this first m.file slice. Actual snapshot
length/SHA256 override no sender observation; they are separately recorded facts.
FileService never exposes the selected path or private root in a safe receipt.

reserve_file_delivery runs in one domain transaction. It binds call_id plus the
complete content digest to the original capability/route, inserts immutable
filename/caption/MIME policy and invokes the existing original upload reservation.
Only the first committed reservation returns preparation custody. An identical
replay returns the same delivery receipt with no capture grant; changed path or
metadata conflicts. A lost admission response is uncertain and can be inspected
by the same call_id; it cannot cause another snapshot. queued is returned only
after durable admission. The existing five-second MCP/client budget is retained;
the tool does not wait synchronously for upload or event delivery.

The worker retains original source/ancestor custody, encryption and PreparedEncrypted
across the domain stage-binding await. It commits the actual namespace, operation,
receipt digest and length before stage_prepared writes. It then uses actual
qualified restoration and the exact existing claim/Send to enter ADR089. There is
no second encryption after queue failure, cancellation, lost acknowledgement or
restart. Staging uncertainty quarantines new capture until an explicit retained
owner inspection resolves it. Local IO has no claimed hard kernel deadline;
driver timeout preserves uncertainty and ownership instead of aborting a thread.

The durable delivery table retains at most 4096 rows globally and 16 per dispatch,
including unknown and cancelled rows. Its fixed per-row metadata/receipt encoding
limit is 8 KiB; paths, ciphertext, keys and SDK bodies are excluded. Replays and
exact status reads remain possible at capacity. SDK permanent record, event-size,
response and ciphertext limits remain independently enforced. These are logical
payload/record limits, not a promise about RSS, TLS buffers or disk availability.

## Separate upload and file-event state

Schema020 is proposed as an additive file-delivery table referencing the original
file_uploads row. It stores immutable metadata and source request commitment,
actual captured size/hash once observed, original route, cancellation, a stable
file-event transaction ID and separate event phase/fence/receipt. Schema019 upload
states remain unchanged. Old upload rows have no reconstructed delivery metadata
or event permission after migration.

The event phase is Pending, Claimed, WritePossible or Delivered. Known pre-event
refusal/cancellation is orthogonal to phase; any possible event write remains
unknown until exact accepted evidence arrives. Claim/begin/validate methods are
distinct from final-reply Done gates and from validate_upload_send. New publication
requires the same original current dispatch/task/lease and frozen route plus
historical Accepted upload. Upload acceptance clears its own send token; the file
event must not reuse or reconstruct that token as permission.

The proposed Matrix entry is synchronous admission returning a finite retained
FilePublicationOperation from an actual ADR089 UploadOperation and an opaque
FilePublicationClaim. Failure returns original inputs. The actual upload operation
still owns its original RestoredEncrypted descriptor after successful run; no raw
descriptor/MXC/body constructor is added. The publication owner retains that same
large job slot, or transfers it without release, through dropped futures and SDK
replacement. It cannot double the pool by copying ciphertext. FileService holds
the operation and drives it; HTTP/MCP cancellation cannot discard custody.

The SDK verifies the exact accepted upload record, original SDK/config identity,
upload ID/fence/stage, and immutable publication metadata before preparing content.
It joins the original descriptor with that record's checked MXC inside private SDK
custody. A digest/string or domain Accepted flag alone cannot supply media evidence.
The encrypted m.file contains exact filename, optional caption, actual size,
original descriptor and MXC; the original thread relation is preserved. Descriptor,
MXC, keys, caption and private root data stay out of public domain receipts and
tool diagnostics. Safe filename and actual size/hash may appear in the scoped
tool receipt described below.

Extend the existing outgoing Kind/Source with a dedicated file variant and reuse
its bounded recipient preflight, SDK encryption and exact transaction custody.
File publication must traverse outgoing_preflight, including CURRENT-token whoami,
current encrypted room membership and recipient-key checks even when an SDK owner
already exists. Observed negative identity/privacy evidence retains bounded owned
fencing, including ADR091's enqueued writer rule. Release upload/SDK locks during
upload network waits as ADR089 requires; publication must not prevent independent
host invalidation from reaching the domain writer.

After the final SDK Possible acknowledgement and immediately before each actual
HTTP write, revalidate the original file-event claim against the domain writer.
No intervening await may substitute different authority. Persist exact accepted
event response in SDK custody before historical domain Delivered. Upload acceptance,
room acceptance, recipient download, canonical Done and process cleanup remain
separate observations. delivered means the room event was acknowledged, not that
the human opened the attachment.

## Status, restart, cancellation and shutdown

send_file returns a small closed receipt: delivery_id, status, upload/event phase,
safe filename, actual size/hash when known, event_id only after acceptance, and a
fixed error_code when appropriate. Status is queued, delivered, failed or
outcome_unknown. failed requires a known terminal refusal; possible upload/event
writes cannot be mapped to failed or silently retried. No path, descriptor, MXC,
key, token, raw HTTP body or debug representation appears in MCP/HTTP output.
get_file_delivery selects only a receipt belonging to the caller's authorized
original conversation/task scope and never starts work. Query selectors are data,
not execution authority; foreign and missing selectors have indistinguishable
refusal. Tool completion does not call transition_task or complete_task_with_reply.

Restart opens only the actual protected domain and SDK journals. Bounded known
delivery selectors come from the delivery table, not scans of filenames or remote
media. Restart may settle complete SDK upload/event receipts without original
capability/request memory. It must not snapshot again, recreate upload Send,
re-encrypt an uncertain event or poll another POST/PUT. An accepted upload with no
prepared event is not a restored publication grant. Missing metadata/SDK history,
incomplete bootstrap or a retired dispatch leaves an explicit failed/unknown gate.
No automatic rearm of Pending, Claimed or WritePossible history is introduced.

Shutdown closes admission atomically, signals bounded current work and preserves
possible effects until owned worker/SDK completion or an explicit unknown result.
No force-kill of a blocking thread, premature slot release, hidden successful
close or dropped last-owner result is permitted. A service may report shutdown
incomplete while its retained worker still owns IO; the production shutdown policy
must expose that limitation and must not claim a durability guarantee.

## Required proof and remaining qualification

The mandatory integration starts the actual native service bootstrap with fresh
private state, an owned offline native child and the real hagency mcp executable.
A local TLS Matrix peer supplies authenticated SDK events/keys and real media/event
acknowledgements. The peer SDK decrypts the delivered m.file and its uploaded bytes.
Assertions cover original content/name/thread, a single media POST, separate room
event acceptance, exact MCP status, no secret projection and unchanged task state.
Starting App only in a test with substitute file handlers is insufficient.

Other required selectors prove same-root launch and source access under the selected
host-exclusive profile and refusal of supported substitution cases, relative
path/symlink/hardlink refusal before capture, queue retirement, content-bound replay,
slot retention after caller loss, cancellation between upload and event, current
token replacement, post-Possible invalidation, SDK/domain failure and fresh-process
settlement with zero repeated HTTP writes. Startup/config and capability tests
prove omission/unsupported qualification remains unavailable. Their final strict
lifecycle must include every actual path and executable selector with no invented
passing scenarios. Proposed/unimplemented selectors are not test results.

Linux, macOS and Windows source-root binding need their own actual native fixtures
under the documented trust profile. A hostile same-UID replacement fixture cannot
silently strengthen that profile or claim unsupported runtime namespace protection.
Windows FileSyncedDirectoryUnconfirmed must still refuse positive staged upload
and report exact returned custody; that refusal cannot pass the positive encrypted
delivery selector. Where Windows durability remains unqualified, the vertical
platform acceptance remains unresolved even if cross-compilation succeeds. Actual
model sandbox/approval application, full runtime qualification, arbitrary room
policy, receive caches, image previews, larger byte limits and production cutover
remain explicit separate gates. This proposal claims no completed migration phase.
