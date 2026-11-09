spec: task
name: "Validate and install exact-commit language-neutral capability packages"
inherits: project
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
tags: [active, rust, plugins, security]
---

## Intent

Define the closed manifest schema, exact GitHub source identity, immutable
package revisions, service-owned registry transaction, and lease-safe lifecycle.

## Constraints

### Must
- Parse only `schema = "hagency.plugin/v1"` and the seven named capability
  versions. Require bounded IDs, SemVer, argv arrays, relative entrypoints, and
  no unknown fields.
- Reject duplicate IDs/entrypoints, symlinks, path escapes, unsupported
  platform/version, submodules, and digest drift. Validate every command's first
  argv against its capability rule at invocation.
- Require lowercase full commit refs and isolated detached system-git checkout.
- Require exact trust before build; copy, fsync, reparse, and rehash the package;
  activate only by an atomic switch to a new digest-named directory.
- Migrate `hagency_plugin_registry_v1` in memory to v2 before serving mutations.
  Persist `PluginRecord { id, source, enabled, activePackageSha256, revisions }`
  and immutable `PluginRevision` metadata.
- Preserve the active revision on every failed preview/apply/update/cancel path;
  retain old revisions while leases are held and collect only the exact obsolete
  managed revision after the last lease drops.
- Keep linked sources authoring-only and re-snapshot them before every
  invocation. Disable/update/uninstall revoke matching qualification rows.

### Must Not
- Do not load dynamic libraries, WebAssembly, ambient plugins, branch heads,
  archives, submodules, symlinks, or a second downloader.
- Do not place plugin state in the built-in fleet runtime file.

## Boundaries

### Allowed Changes
- native/hagency-plugin/**
- native/hagency-store/src/migrations/**
- Cargo.toml
- Cargo.lock
- specs/task-rust-plugin-package.spec.md

### Forbidden
- provider credentials, user plugin state, deployed services

## Acceptance Criteria

Scenario: A manifest is a closed bounded schema
  Test: native_plugin_manifest_accepts_closed_schema
  Given valid and malformed manifests
  When they are parsed
  Then only the exact schema, fields, versions, bounds, and entrypoint rules pass

Scenario: GitHub refs are exact and detached
  Test: native_plugin_github_source_requires_full_commit
  Given branch, short, uppercase, malformed, and full lowercase refs
  When a source is parsed
  Then only the full lowercase 40-hex commit passes

Scenario: Registry switching is atomic and lease-safe
  Test: native_plugin_registry_preserves_active_on_failed_apply
  Given an active revision and a failed replacement
  When the service transaction is applied
  Then the old revision remains active and readable while leased

Scenario: Linked sources stay authoring-only
  Test: native_plugin_linked_source_never_builds_or_collects
  Given an absolute local package
  When it is linked and later updated/collected
  Then the source is recorded, re-snapshotted for invocation, and never garbage-collected by Hagency

## Out of Scope

Capability execution and runtime qualification are covered by the other plugin
contracts.
