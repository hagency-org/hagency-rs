# Agent names and retirement cleanup (operator runbook)

## Reusing an agent name

- A name is held only by a `pending`, `reserved` or `active` engagement in the same project. After you retire an engagement (state `revoked`) the name is free at once. A new request gets a new engagement id and a new Matrix identity.
- Refusal `agent_name_in_use`: a live engagement holds the name. The message names the engagement and its state. Retire that engagement (console Engagements, Retire) or choose another name.
- Refusal `request_conflict` for a name whose engagements are all revoked does not come from the Hagency name check. The Palpo coordinator and Rinx request path are not part of Hagency, so check them there.
- Retirement cleanup staying `uncertain` does not hold the name.

## Retirement cleanup state

- Palpo's `POST {fleet endpoint}/retire-agent` returns 503 until Palpo implements it. The retire effect stays pending and is never marked applied without Palpo's verified receipt.
- Hagency retries each effect with backoff: 5 s, doubling, capped at 10 minutes, reset on success. The first failure and each change of failure kind log at WARN; repeats log at DEBUG.
- The console Engagements page shows, on a revoked row, the attempt count, last error and next attempt time (`GET /console/api/engagements/cleanup-state`). The state is in memory and restarts with the process.

## A provision stuck after the owner invite

- The console agent row shows `provision_reason` and `provision_remedy` for `awaiting_owner`, `provision_retrying` and `provision_parked`. Read the reason first.
- `owner_not_joined`: the owner accepts the invitation to the named room. `transport_retrying`: wait, no action; retries run 5 s doubling to 5 min, at most 6. `transport_retry_exhausted`: check the Matrix/Palpo server; the service looks again after 5 minutes.
- `owner_membership_not_provable`: the owner accepts the invitation, then the next pass proves it.
- `enrollment_not_provable` or `account_not_provable`: the state cannot be proven and keys are never re-created. Retire the agent from the console only if the operator check finds no fix.
- Resume needs no command: `provision_pass` and the restart path both use `resume_enrolled`. Never edit SQLite.
