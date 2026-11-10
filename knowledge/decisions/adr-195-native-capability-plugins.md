---
kind: decision
id: ADR-195
title: "Run exact-commit capability plugins out of process under Hagency custody"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
amends: [ADR-189, ADR-192, ADR-193]
tags: [native, plugins, security, runtime, harness]
---

## Context

Hagency needs implementation-language-neutral extensibility for operator actions,
events, approval-gated tools, runtime adapters, and external harness views. The
production topology is Rinx → Palpo → Hagency. Rinx is the user interaction
surface; Palpo is the homeserver/fleet transport; Hagency owns the local fleet,
plugin lifecycle, execution authority, usage, and settlement. Herdr provides
optional runtime/harness adapter examples, not architecture authority.

## Decision

1. The running Hagency service is the sole plugin owner. It owns one registry
   handle, scheduler, immutable revision leases, previews, invocation table,
   event queues, startup sessions, harness sessions, qualification runner, and
   shutdown. CLI, console, and operator routes call this service; none opens a
   second registry or writes plugin state offline.
2. `hagency-plugin.toml` is a closed `hagency.plugin/v1` schema. Its only
   capability versions are `startup.v1`, `action.v1`, `event.v1`, `tool.v1`,
   `runtime.v1`, `harness.v1`, and `pane.v1`. Startup/action/event are one-shot
   commands; tools require owner approval; runtime adapters own admitted agent
   turns; harnesses receive redacted views/control receipts and may prepare or
   release only a target named in the frozen template; panes are declarative
   metadata delivered to their named harness.
3. Remote source is GitHub at an exact lowercase 40-hex commit. Hagency uses an
   isolated detached checkout, rejects submodules/symlinks/escapes, requires
   explicit trust, builds in staging, reparses and hashes the copy, and switches
   the immutable revision atomically. Linked sources are authoring-only and are
   re-snapshotted before invocation.
4. The private `hagency_plugin_registry_v2` records one active digest plus all
   retained immutable revisions. Old revisions remain readable while leased and
   are collected only after their final lease drops. A failed domain revocation
   marks the plugin stale and refuses new launches until reconciliation.
5. Every plugin command uses the existing guardian and bounded scheduler. The
   child environment is allowlisted and contains no Matrix, App Service,
   operator, provider, runner, or credential secret. Plugins remain trusted
   user-level executables with OS filesystem/network access; this is not a
   sandbox.
6. Hagency owns task state, approval, workspace custody, usage, canonical
   results, cleanup, and settlement. Tool approvals are `Approve once` or
   `Deny`; tool custody is keyed by dispatch/call identity and replay never
   launches twice. Runtime results, harness views, and event output cannot
   create, approve, complete, or settle work. Harnesses cannot submit turns.
7. Runtime publication and dispatch re-read exact enabled qualification identity
   for plugin, adapter, package, framework, model, provider, and reasoning.
   Qualification executes the active revision through the normal session seam;
   Hagency derives the evidence digest and rejects caller-authored evidence.
8. `hagency.harness/v1` is host-supervised. It exchanges `initialize`,
   `snapshot`, `event`, `shutdown`, `agent/prepare`, and `agent/release`; view
   controls are limited to `stop_attempt` and `open_console`. Prepare/release
   act only on the template-selected target; managed mode starts/stops that
   target. Harnesses do not submit turns. Hagency never hosts plugin HTML,
   JavaScript, or a PTY. Herdr adapters are optional examples.

## Consequences

Good: third-party implementations can use any language that speaks the bounded
argv/JSON protocols, while one host retains lifecycle, authority, custody, and
settlement. Content-addressed revisions make update and replay behavior
inspectable.

Bad: plugin code still has the OS user's filesystem/network permissions; real
qualification and guardian cleanup are required; runtime changes can require a
restart of warm hosts; plugins cannot host arbitrary UI or assume Matrix access.

## Alternatives Considered

- **Herdr as the coding-agent executor:** rejected; it would create a second
  approval, result, usage, and cleanup authority.
- **Dynamic libraries, WebAssembly, or ambient discovery:** rejected; Hagency
  could not prove the lifecycle and authority boundary.
- **Branches, archives, or a marketplace:** rejected; v1 needs stable exact
  source identity without a second downloader or dependency installer.
- **Plugin-authored console HTML/JavaScript:** rejected; it expands the trusted
  UI/session surface and bypasses host-authored authority checks.

## Implementation Notes

The seven capability schemas, scheduler limits, stable refusal codes, wire
protocols, registry migration, invocation custody, event publisher, and
qualification matrix are bound by the six plugin task contracts. Built-in
Codex/Claude/Octos resources remain independent and continue to work when a
plugin is disabled, stale, or unavailable. No plugin receives a Matrix,
App Service, operator, provider, or runner credential.
 
## 2026 trust-flow amendment

Remote sources remain GitHub-only but accept canonical HTTPS and SSH transports. Every remote source carries an exact lowercase 40-hex commit and an optional safe relative subdirectory; branches, tags, abbreviated commits, archives, submodules, file URLs, redirects to another host, embedded credentials, and arbitrary Git hosts remain refused.

Preview is read-only and returns the normalized source plus source, manifest, and trust digests. Install resubmits that normalized source and trusted digest, prepares a fresh isolated checkout, verifies the digest before build or registry mutation, and has no preview ID or durable preview cache. The running Hagency service remains the sole registry writer; this removes request affinity, not the single-owner registry model.

SSH uses only the service account's existing agent and readable `known_hosts`; no private-key value or path is accepted, persisted, or rendered. Authentication prerequisites have a stable unavailable refusal. The host-authored console owns the source-review-to-install-to-settings-to-enable-to-qualification flow and never renders plugin-authored UI.

The package and management contracts bind exact source normalization, stateless install across `PluginService::open`, source-change refusal, atomic failure recovery, and the browser lifecycle flow.
