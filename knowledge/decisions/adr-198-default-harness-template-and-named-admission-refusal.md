---
kind: decision
id: ADR-198
title: "Provision only agents that can run: default harness template and named refusal"
status: Accepted
satisfies: [REQ-NATIVE-PLUGIN-SYSTEM]
amends: [ADR-190, ADR-195]
tags: [native, plugins, provisioning, harness, diagnostics, palpo]
---

## Context

Rinx request-agent (`apps/palpo/bundle/main.splash`) sends only
`agentDefinition {name, resourceId}`, role, tokens and rate. Hagency selects the
harness template with `SELECT harness_template FROM agent_definitions WHERE
resource_id AND name AND role AND enabled` (`hagency-store/src/domain.rs`, near
line 2416). On 2026-11-16 the agent `qore-orchestrator` was requested with no
matching definition, so `harnessTemplate` was null. Every dispatch then failed
in host admission: `effective_harness_template` calls
`resolve_harness_template` with empty values, which fails, and the result is
`Failure::Admission` (`hagency-execution/src/host.rs`). The room showed only
"Interrupted; the result is not confirmed". The template is frozen at
provisioning, so the engagement could not be repaired and had to be retired.

## Decision

1. **Resolve at provisioning, in this order.** When the resource has a harness
   plugin, provisioning resolves the template (a) from the enabled agent
   definition matching resource, name and role; else (b) from a resource-level
   default harness template set by the operator; else (c) it refuses. The
   resolved template is validated against the harness plugin
   (`validate_harness_template`) before the engagement is reserved. Resolving
   from empty values at dispatch time stays only for harnesses whose
   configuration needs no required setting; at provisioning it is made explicit
   instead of falling to dispatch.
2. **Resource default template.** The operator can set and clear one default
   harness template per resource through the resource configuration route
   (console and operator CLI). It is validated like a definition, versioned by
   its `template_sha256`, and the sha is recorded in the engagement's provision
   effect so the choice is auditable. A later change to the default never alters
   existing engagements; it applies through the ADR-197 rebind path only.
3. **Up-front refusal with a named reason.** If no template resolves, the
   request is refused before any engagement, identity or room exists. The error
   is a stable named code: `harness_template_missing` (no definition, no
   default) or `harness_template_invalid` (a template exists but fails
   validation, with the first failing setting named). The code and a one-line
   operator fix ("create agent definition <name> for role <role> on resource
   <id>, or set a default harness template") reach the requester: the Palpo
   fleet request result that Rinx reads, and the console request/failure view.
4. **Admission failures name their cause.** `Failure::Admission` carries a
   reason enum (for example `harness_template_missing`,
   `harness_template_mismatch`, `plugin_package_not_launchable`,
   `plugin_not_qualified`). The room notice and console show the reason text
   instead of only "Interrupted; the result is not confirmed". The notice never
   includes settings values or secrets. Admission failures stay non-redoing: the
   message says nothing was sent.
5. **Existing broken engagements.** An engagement already provisioned with a
   null template is shown in the console as `cannot run: harness_template_missing`
   and repaired through the ADR-197 rebind path (a rebind may bind a resolved
   template), not by editing the frozen effect.

## Consequences

Good, because a request that can never run is refused where the requester sees
it, with the fix named.
Good, because the room and console say why a dispatch was refused.
Bad, because requests that used to be accepted and then failed silently are now
refused; fleets relying on null templates with a harness plugin must add a
definition or default. Resources with no harness plugin are unchanged.

Rollback: the up-front refusal can be switched off by a flag, which restores the
old behavior while keeping the named admission reasons.

## Alternatives Considered

- Fall back to an empty-values template silently: rejected, it is the failure
  seen on 2026-11-16 when a required setting exists.
- Let Rinx send a template: rejected, templates are operator-owned and reviewed
  (ADR-190 keeps the request minimal).
- Allow editing the frozen template: rejected, it breaks the provision-effect
  fingerprint.

## Tests later steps must create

- `provision_resolves_template_from_matching_definition`
- `provision_falls_back_to_resource_default_template`
- `provision_refused_harness_template_missing_before_any_identity`
- `provision_refused_harness_template_invalid_names_source`
- `provision_refusal_reaches_palpo_request_result`
- `resource_default_template_change_does_not_alter_existing_engagements`
- `host_admission_failure_carries_named_reason`
- `room_notice_names_admission_reason_without_secrets`
- `console_marks_engagement_with_null_template_cannot_run`
