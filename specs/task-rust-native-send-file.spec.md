spec: task
name: "Expose native send_file through retained workspace upload and encrypted delivery"
inherits: project
satisfies: [REQ-RUST-MIGRATION-EXECUTION, REQ-THREAD-SCOPED-SESSIONS, REQ-MATRIX-DM-PRIVACY, REQ-THREE-LAYER-COMPLETION]
tags: [proposed, rust, matrix, files, mcp, integration]
---

## Intent

Implement ADR092 only after root review accepts it and ADR093 supplies actual
shared physical workspace custody. Complete the native service and MCP path from
an assigned source file to an acknowledged encrypted Matrix file event.
This proposal contains unimplemented selectors and authorizes no production edits
before review. It must not be integrated alone as a passing implementation.

## Constraints

### Must
- Start the same finite FileService through native serve bootstrap and real MCP routes used by the executable integration fixture.
- Bind the actual ADR093 retained source root and runner cwd to the exact Started dispatch under the accepted host-exclusive directory and ancestor lifetime profile.
- Revalidate current domain scope after every staging queue wait before capture and after final SDK Possible acknowledgement before network writes.
- Persist immutable original metadata and original upload reservation together before capture and bind actual PreparedEncrypted identity before storage IO.
- Keep two retained file jobs and one owned blocking staging worker with no queue beyond admitted slots across dropped requests and SDK reopen.
- Consume actual ADR089 upload custody and private accepted SDK evidence to prepare encrypted file content without public descriptors or MXCs.
- Keep upload acceptance event delivery cancellation canonical Done and physical cleanup as independent states.
- Use existing current-token outgoing preflight and retain observed negative fencing through caller loss and enqueued domain completion.
- Preserve original capture encryption event transaction and unknown outcomes across exact request replay and restart.
- Advertise the actual 4 MiB initial product limit and retain existing tighter component limits.

### Must Not
- Do not turn a logical workspace ID path equality serialized file identity or duplicate ambient open into physical root authority.
- Do not accept a model-selected destination runtime command host configuration credential or permission override.
- Do not return queued before durable admission or delivered before actual room-event acceptance.
- Do not reissue upload Send re-encrypt or repeat possible POST or PUT after failure cancellation reopen or process loss.
- Do not expose source paths descriptors MXCs keys capabilities raw responses or private SDK metadata in safe output.
- Do not claim Windows positive durability runtime sandbox qualification production availability or migration parity from refusal branches or cross-compilation.

## Boundaries

### Allowed Changes
- ./Cargo.lock
- native/README.md
- native/hagency/Cargo.toml
- native/hagency/src/main.rs
- native/hagency/src/lib.rs
- native/hagency/src/bootstrap.rs
- native/hagency/src/file_service.rs
- native/hagency/src/file_service/config.rs
- native/hagency/src/file_service/worker.rs
- native/hagency/src/file_service/state.rs
- native/hagency/src/runner.rs
- native/hagency/src/runner/files.rs
- native/hagency/src/mcp.rs
- native/hagency/src/mcp/catalog.rs
- native/hagency/src/mcp/files.rs
- native/hagency/src/task_client.rs
- native/hagency/src/task_client/transport.rs
- native/hagency/src/task_client/files.rs
- native/hagency-runtime/src/codex/session/task_mcp.rs
- native/hagency-core/src/lib.rs
- native/hagency-core/src/file_delivery.rs
- native/hagency-store/src/domain.rs
- native/hagency-store/src/domain_worker.rs
- native/hagency-store/src/lib.rs
- native/hagency-store/src/domain/uploads.rs
- native/hagency-store/src/domain/file_delivery.rs
- native/hagency-store/src/migrations/020-file-deliveries.sql
- native/hagency-matrix/src/lib.rs
- native/hagency-matrix/src/collector.rs
- native/hagency-matrix/src/upload.rs
- native/hagency-matrix/src/upload/state.rs
- native/hagency-matrix/src/file_publication.rs
- native/hagency-matrix/src/file_publication/state.rs
- native/hagency-matrix/src/outgoing.rs
- native/hagency-matrix/src/outgoing/state.rs
- native/hagency-matrix/src/sdk.rs
- native/hagency-matrix/src/sdk/outgoing.rs
- native/hagency-matrix/src/sdk/upload_custody.rs
- native/hagency-matrix/src/upload_custody.rs
- native/hagency/tests/native_file_service.rs
- native/hagency/tests/native_file_service/fixture.rs
- native/hagency/tests/native_file_service/recovery.rs
- native/hagency/tests/native_file_service/scope.rs
- native/hagency/tests/fixtures/owned_mcp_peer.rs
- native/hagency/tests/mcp.rs
- native/hagency/tests/http.rs
- native/hagency/tests/cli.rs
- native/hagency/tests/owned_mcp.rs
- native/hagency-store/tests/file_delivery.rs
- native/hagency-store/tests/migrations.rs
- native/hagency-store/tests/owned_completion.rs
- native/hagency-store/tests/approvals.rs
- native/hagency-store/tests/file_uploads.rs
- native/hagency-store/tests/verified_ingress/attachments.rs
- native/hagency-store/tests/usage.rs
- native/hagency-store/tests/replies.rs
- native/hagency-store/tests/workflows/custody.rs
- native/hagency-store/tests/verified_ingress/notice_custody.rs
- native/hagency-store/tests/conversations.rs
- native/hagency-store/tests/workflows/mod.rs
- native/hagency-matrix/tests/file_publication/mod.rs
- native/hagency-matrix/tests/file_publication/fixture.rs
- knowledge/decisions/adr-092-native-send-file-service.md
- specs/task-rust-native-send-file.spec.md
- docs/agent-knowledge.md
- docs/progress.md

### Forbidden
- Physical-root implementation files remain owned by ADR093 and must be integrated as a prerequisite rather than changed under this contract.
- Existing JS production files generated workspace entry files website code service installations live credentials and live runtime state are outside this proposal.

## Acceptance Criteria

Scenario: Native service and MCP deliver actual encrypted original bytes
  Level: integration
  Test Double: actual native service and MCP executables owned offline child local TLS peer and independent real SDK recipient
  Test: native_send_file_service_mcp_delivered
  Given a private configured native service and a current owned dispatch with an actual ADR093 bound workspace
  When the native MCP send_file and get_file_delivery tools drive one original source through staging upload and room publication
  Then the recipient decrypts exact original bytes filename caption and thread after one media POST and actual room-event acknowledgement
  And queued precedes delivered only after durable admission while canonical task state remains unchanged and safe output contains no private material

Scenario: Startup cannot override absent physical or runtime qualification
  Level: integration
  Test Double: real executable startup with disposable private config files and fixed unsupported qualification inputs
  Test: native_send_file_bootstrap_refuses_unqualified
  Given missing malformed foreign or oversized configuration or unqualified runtime and root custody
  When serve initializes its file service or an unavailable tool is requested
  Then admission remains unavailable without opening foreign state spawning a runner or accepting file work

Scenario: Actual runner cwd and source keep the same retained root
  Level: integration
  Test Double: real guardian or Windows child launch retained source handles and documented host-exclusive root lifetime
  Test: native_send_file_same_root_service
  Given an actual retained source root registered to the exact dispatch and a host maintaining its exclusive root and ancestor lifetime
  When the real native child runs and the service captures its relative file selection
  Then the child cwd and file source use that original host binding and supported substitution cases refuse before replacement content is read
  And neither canonicalized descriptor aliases nor this trusted-host fixture claim hostile same-UID runtime namespace isolation

Scenario: Model selections cannot choose external files or destinations
  Level: integration
  Test Double: real native MCP and service requests against disposable symlink hardlink and foreign-scope fixtures
  Test: native_send_file_scope_refusals
  Given invalid relative selections object aliases foreign capabilities or caller-selected destination fields
  When send_file reaches the actual service route
  Then it refuses before source capture staging or HTTP and returns only a fixed safe refusal

Scenario: Stable requests preserve one original capture
  Level: integration
  Test Double: actual source mutation and repeated MCP calls sharing one domain writer
  Test: native_send_file_content_bound_replay
  Given a durable original file admission with fixed metadata and request digest
  When the source changes and identical or changed requests reuse its call_id
  Then identical requests inspect original custody without another snapshot and changed content conflicts

Scenario: Queue retirement refuses capture and exhausted custody refuses admission
  Level: integration
  Test Double: bounded worker gates real domain revocation and multiple actual service clients
  Test: native_send_file_retained_worker_bounds
  Given two held jobs and an original job paused before capture
  When clients drop requests SDK ownership reopens or current scope retires during the queue wait
  Then held capacity cannot reset excess admission refuses and retired queued work never reads source bytes

Scenario: Persisted metadata and upload reservation have one transaction
  Level: integration
  Test Double: actual SQLite transaction failure triggers and repository reopen
  Test: native_file_delivery_metadata_atomic
  Given an original capability and bounded file metadata
  When admission commits rolls back or loses its response
  Then metadata and upload reservation agree atomically and replay never returns another preparation grant

Scenario: Accepted upload does not grant event delivery or task completion
  Level: integration
  Test Double: actual successful local TLS upload followed by cancellation and private route retirement
  Test: native_send_file_upload_event_separation
  Given actual private SDK and domain upload acceptance with no room-event acknowledgement
  When publication is cancelled or the original route retires
  Then the upload remains historically accepted no new file event begins and status never reports delivered or canonical Done

Scenario: New publication authenticates current token and current final authority
  Level: integration
  Test Double: actual SDK reopen current-token whoami mismatch and a deterministic pause after SDK Possible
  Test: native_file_publication_current_authority
  Given historical upload acceptance an existing SDK owner and a separate original file-event claim
  When current token identity changes or the route retires after the final SDK write acknowledgement
  Then retained negative fencing survives caller drop and no subsequent event HTTP write starts

Scenario: Private media association refuses substituted input
  Level: integration
  Test Double: two actual uploads with distinct protected SDK records original descriptors and metadata
  Test: native_file_publication_exact_media
  Given two same-fence uploads with different original scope stage or metadata
  When publication input attempts to associate one accepted response with the other original operation
  Then admission returns original custody and no event is prepared or sent

Scenario: Caller loss and persistence failures never repeat possible writes
  Level: integration
  Test Double: actual paused media and room HTTP requests SQLite failure triggers and retained service owners
  Test: native_send_file_cancel_and_response_custody
  Given a possible upload or publication with a response before or after private persistence
  When the MCP client disconnects the operation is cancelled or persistence fails
  Then original finite custody and exact complete evidence remain available without another POST or PUT

Scenario: Fresh processes settle actual accepted history without original secrets
  Level: integration
  Test Double: separate real service processes protected journals exact selectors and an idle local TLS peer
  Test: native_send_file_process_recovery_no_write
  Given SDK upload or room-event acceptance followed by a failed first domain receipt commit
  When all original owners and capabilities are dropped and a new service opens the exact history
  Then first historical settlement and replay succeed without original capability request memory or another network write

Scenario: File service shutdown preserves incomplete ownership
  Level: integration
  Test Double: actual retained worker delayed storage completion and simultaneous service admission
  Test: native_send_file_shutdown_custody
  Given active or unknown jobs during service shutdown
  When admission closes and the bounded shutdown observation expires
  Then new work refuses while original ownership remains retained and no false successful close is reported

Scenario: Unsupported Windows durability remains an unresolved positive gate
  Level: integration
  Test Double: actual native Windows private staging with its observed directory synchronization result
  Test: native_send_file_windows_durability_refusal
  Given FileSyncedDirectoryUnconfirmed from actual Windows staging
  When the host attempts the encrypted file workflow
  Then exact original custody is returned no upload or event HTTP begins and the result is not recorded as a positive delivery qualification

## Out of Scope

ADR093 implements physical-root mechanics and its platform proofs separately.
Live services live models automatic runtime enablement effective sandbox proof
approval application receive caches image previews plaintext rooms new room
discovery larger product limits and production cutover remain separate gates.
Positive service acceptance must execute on a qualified platform; a Windows refusal
or cross-compilation cannot substitute for the positive delivery selector.
