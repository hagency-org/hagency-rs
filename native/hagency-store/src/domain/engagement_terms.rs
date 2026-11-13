//! Task #19 TS parity: role offers (count/budget/rate caps), the project room
//! whitelist, per-resource agent definitions, and the admission routing that
//! TS enforced at engagement admission (backend-v2.js:15330-15385,
//! 15802-15833, 15960-15971; lib/engagement-store.js:178-232, 396-478;
//! lib/resource-agent-definitions.js).
//!
//! TS records an over-cap or untrusted request instead of refusing it — the
//! route names why the request did not auto-join — so these methods never
//! refuse on caps either; they refuse only on malformed input, missing rows,
//! or the two delete guards TS defined.
use super::*;
use hagency_plugin::{HARNESS_TEMPLATE_SCHEMA, HarnessTemplateDocument, harness_template_digest};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentDefinitionInput {
    pub name: Option<String>,
    pub role: Option<String>,
    pub enabled: Option<bool>,
    /// `None` preserves an existing override; a supplied document replaces it.
    #[serde(default)]
    pub harness_template: Option<HarnessTemplateDocument>,
}
/// TS lib/engagement-store.js:37-38.
fn valid_room_id(room: &str) -> bool {
    let Some((local, server)) = room.split_once(':') else {
        return false;
    };
    !local.is_empty()
        && local.starts_with('!')
        && !server.is_empty()
        && !room.chars().any(char::is_whitespace)
}
fn valid_mxid(mxid: &str) -> bool {
    let Some((local, server)) = mxid.split_once(':') else {
        return false;
    };
    !local.is_empty()
        && local.starts_with('@')
        && !server.is_empty()
        && !mxid.chars().any(char::is_whitespace)
}
/// TS lib/engagement-store.js posInt: floor first, then require a positive
/// integer below 2^53 — a cap that cannot be compared is refused, not stored.
fn pos_int(value: i64, field: &'static str) -> Result<i64, Error> {
    if value <= 0 || value > 9_007_199_254_740_991 {
        return Err(hagency_core::InvalidInput(field).into());
    }
    Ok(value)
}
/// TS lib/resource-agent-definitions.js name rule.
fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 64
        && name
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}
fn validate_harness_template(
    resource: &Resource,
    value: &HarnessTemplateDocument,
) -> Result<HarnessTemplateDocument, Error> {
    if value.schema != HARNESS_TEMPLATE_SCHEMA {
        return Err(hagency_core::InvalidInput("invalid harness template").into());
    }
    let Some(identity) = resource.harness_plugin.as_ref() else {
        return Err(Error::Unqualified);
    };
    if value.plugin_id != identity.plugin_id
        || value.package_sha256 != identity.package_sha256
        || value.settings_sha256 != identity.settings_sha256
    {
        return Err(Error::Conflict);
    }
    hagency_core::project::identifier(&value.harness_id, 128)?;
    for digest in [
        &value.package_sha256,
        &value.settings_sha256,
        &value.template_sha256,
    ] {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(hagency_core::InvalidInput("invalid harness template digest").into());
        }
    }
    if serde_json::to_vec(value)?.len() > 1024 * 1024 {
        return Err(Error::Capacity);
    }
    if harness_template_digest(value)? != value.template_sha256 {
        return Err(Error::Conflict);
    }
    Ok(value.clone())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleOffer {
    pub role: String,
    pub count: Option<i64>,
    pub budget_cap_per_engagement: Option<i64>,
    pub rate_cap: Option<i64>,
    pub published: bool,
    pub updated_at: Option<u64>,
    pub updated_by: Option<String>,
}
pub(super) fn read_offer(db: &Connection, role: &str) -> Result<Option<RoleOffer>, Error> {
    Ok(db
        .query_row(
            "SELECT role,count,budget_cap_per_engagement,rate_cap,published,updated_at,updated_by \
             FROM role_offers WHERE role=?1",
            [role],
            |r| {
                Ok(RoleOffer {
                    role: r.get(0)?,
                    count: r.get(1)?,
                    budget_cap_per_engagement: r.get(2)?,
                    rate_cap: r.get(3)?,
                    published: r.get::<_, i64>(4)? == 1,
                    updated_at: r
                        .get::<_, Option<i64>>(5)?
                        .and_then(|v| u64::try_from(v).ok()),
                    updated_by: r.get(6)?,
                })
            },
        )
        .optional()?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhitelistEntry {
    pub project_room_id: String,
    pub display_name: Option<String>,
    pub added_at: u64,
    pub added_by: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub id: String,
    pub resource_id: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_template: Option<HarnessTemplateDocument>,
}

/// The TS routing decision (lib/engagement-store.js:178-232), ported 1:1.
/// Order is the design: whitelist first, then cross-family, then the offer
/// (promise before ceiling). Pure so the store, the admission path and tests
/// share one vocabulary.
pub fn route_request(
    whitelisted: bool,
    cross_family_ok: bool,
    offer: Option<&RoleOffer>,
    requested_tokens: u64,
    rate_per_day: Option<u64>,
    remaining_tokens: Option<u64>,
    active_for_role: i64,
) -> (&'static str, bool) {
    if !whitelisted {
        return ("notWhitelisted", false);
    }
    if !cross_family_ok {
        return ("crossFamilyUnavailable", false);
    }
    // No offer — or an unpublished one — is NOT on offer: never unlimited.
    let Some(offer) = offer.filter(|o| o.published) else {
        return ("overOffer", false);
    };
    if offer
        .budget_cap_per_engagement
        .is_some_and(|cap| requested_tokens > cap as u64)
    {
        return ("overOffer", false);
    }
    // An unstated rate is unknown, not zero: measured against the cap or queued.
    if let Some(cap) = offer.rate_cap {
        match rate_per_day {
            None => return ("overOffer", false),
            Some(rate) if rate > cap as u64 => return ("overOffer", false),
            _ => {}
        }
    }
    if offer.count.is_some_and(|count| active_for_role >= count) {
        return ("overOffer", false);
    }
    match remaining_tokens {
        None => ("overCeiling", false),
        Some(remaining) if requested_tokens > remaining => ("overCeiling", false),
        _ => ("autoJoin", true),
    }
}

impl DomainRepository {
    /// TS GET /api/offers: EVERY role, offered or not — an absent offer is a
    /// real state, and `catalogPublished` is derived from qualifying published
    /// resources (fleetCatalogOffers parity).
    pub fn offers(&self) -> Result<Vec<Value>, Error> {
        let mut query = self
            .db
            .prepare("SELECT config FROM resources WHERE json_extract(config,'$.published')=1")?;
        let resources: Vec<Resource> = query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_, Error>>()?;
        qualification::roles()
            .map(|role| {
                let offer = read_offer(&self.db, role)?;
                // TS fleetCatalogOffers (backend-v2.js:13931-13936): an
                // explicitly unpublished offer suppresses the catalog view too
                // (`offer?.published === false ? [] : ...`), and an unavailable
                // role has nothing on offer; `role_available` is the native
                // analogue of roleCrossFamilyAvailable's publication gate.
                let catalog_published = offer.as_ref().is_none_or(|o| o.published)
                    && role_available(&self.db, role, None)?
                    && resources.iter().any(|r| r.qualifies(role));
                Ok(match offer {
                    Some(offer) => json!({"role":offer.role,"count":offer.count,
                        "budgetCapPerEngagement":offer.budget_cap_per_engagement,
                        "rateCap":offer.rate_cap,"published":offer.published,
                        "updatedAt":offer.updated_at,"updatedBy":offer.updated_by,
                        "catalogPublished":catalog_published}),
                    // TS absent-offer default (backend-v2.js:15338-15339): no
                    // `updatedBy` key — the key set differs by state, and
                    // adding it would be inventing a writer that never wrote.
                    None => json!({"role":role,"count":null,"budgetCapPerEngagement":null,
                        "rateCap":null,"published":false,"updatedAt":null,
                        "catalogPublished":catalog_published}),
                })
            })
            .collect()
    }

    /// TS PUT /api/offers/:role → setOffer. Caps absent (null) clear; present
    /// values must be positive integers below 2^53; `published` is true only
    /// for an explicit true (TS `published === true`).
    #[allow(clippy::too_many_arguments)]
    pub fn set_offer(
        &mut self,
        role: &str,
        count: Option<i64>,
        budget_cap_per_engagement: Option<i64>,
        rate_cap: Option<i64>,
        published: bool,
        updated_by: &str,
        now: u64,
    ) -> Result<RoleOffer, Error> {
        qualification::check_role(role)?;
        let count = count.map(|v| pos_int(v, "count")).transpose()?;
        let budget_cap_per_engagement = budget_cap_per_engagement
            .map(|v| pos_int(v, "budgetCapPerEngagement"))
            .transpose()?;
        let rate_cap = rate_cap.map(|v| pos_int(v, "rateCap")).transpose()?;
        let updated_by = if updated_by.is_empty() {
            "operator"
        } else {
            updated_by
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO role_offers(role,count,budget_cap_per_engagement,rate_cap,published,updated_at,updated_by) \
             VALUES(?1,?2,?3,?4,?5,?6,?7) \
             ON CONFLICT(role) DO UPDATE SET count=excluded.count, \
             budget_cap_per_engagement=excluded.budget_cap_per_engagement, \
             rate_cap=excluded.rate_cap,published=excluded.published, \
             updated_at=excluded.updated_at,updated_by=excluded.updated_by",
            params![role, count, budget_cap_per_engagement, rate_cap, published, now, updated_by],
        )?;
        tx.commit()?;
        Ok(read_offer(&self.db, role)?.expect("row just written"))
    }

    /// TS listWhitelist: newest first.
    pub fn whitelist(&self) -> Result<Vec<WhitelistEntry>, Error> {
        let mut query = self.db.prepare(
            "SELECT project_room_id,display_name,added_at,added_by FROM room_whitelist \
             ORDER BY added_at DESC, project_room_id",
        )?;
        let rows = query
            .query_map([], |r| {
                Ok(WhitelistEntry {
                    project_room_id: r.get(0)?,
                    display_name: r.get(1)?,
                    added_at: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                    added_by: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
    pub fn is_whitelisted(&self, room: &str) -> Result<bool, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM room_whitelist WHERE project_room_id=?1",
                [room],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// TS addToWhitelist: audited on add AND update; displayName is for
    /// reading only, trimmed to 256; addedAt survives an update.
    pub fn add_whitelist(
        &mut self,
        project_room_id: &str,
        display_name: Option<&str>,
        added_by: Option<&str>,
        now: u64,
    ) -> Result<WhitelistEntry, Error> {
        if !valid_room_id(project_room_id) {
            return Err(
                hagency_core::InvalidInput("projectRoomId must be a Matrix room id").into(),
            );
        }
        // TS (engagement-store.js:399-400) validates the MXID only when a
        // value was PROVIDED; the absent case falls back to 'operator' with
        // no check.
        let added_by = match added_by {
            Some(by) if !by.is_empty() => {
                if !valid_mxid(by) {
                    return Err(hagency_core::InvalidInput("addedBy must be an MXID").into());
                }
                by
            }
            _ => "operator",
        };
        let display_name = display_name.map(|name| {
            let trimmed: String = name.trim().chars().take(256).collect();
            trimmed
        });
        let display_name = match display_name {
            Some(name) if name.is_empty() => None,
            other => other,
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO room_whitelist(project_room_id,display_name,added_at,added_by) \
             VALUES(?1,?2,?3,?4) ON CONFLICT(project_room_id) DO UPDATE SET \
             display_name=excluded.display_name,added_by=excluded.added_by",
            params![project_room_id, display_name, now, added_by],
        )?;
        tx.commit()?;
        Ok(WhitelistEntry {
            project_room_id: project_room_id.to_owned(),
            display_name,
            added_at: now,
            added_by: added_by.to_owned(),
        })
    }

    /// TS removeFromWhitelist: future requests only — live engagements are
    /// untouched and their ids are returned so the operator sees what still
    /// runs under withdrawn trust.
    pub fn remove_whitelist(
        &mut self,
        project_room_id: &str,
    ) -> Result<(String, Vec<String>), Error> {
        // TS removeFromWhitelist never validates the room shape — only
        // addToWhitelist does (engagement-store.js:398 vs 426-434); here a
        // malformed id is merely absent and answers 404.
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT 1 FROM room_whitelist WHERE project_room_id=?1",
                [project_room_id],
                |_| Ok(()),
            )
            .optional()?;
        if existing.is_none() {
            return Err(Error::NotFound);
        }
        tx.execute(
            "DELETE FROM room_whitelist WHERE project_room_id=?1",
            [project_room_id],
        )?;
        // The room lives in the serialized request context (`targetRoomId`),
        // not a column — the engagement projection carries it too, but the
        // context row is the one every writer sets at INSERT time.
        let still_active = {
            let mut query = tx.prepare(
                "SELECT id FROM engagements WHERE json_extract(context,'$.targetRoomId')=?1 \
                 AND state='active' ORDER BY id",
            )?;
            query
                .query_map([project_room_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok((project_room_id.to_owned(), still_active))
    }

    /// TS DELETE /api/framework-presets/:id guards, in TS order: definitions
    /// first, then a reserved agent (an engagement holding this resource).
    /// TS blocked `pending` engagements carrying a fulfillment; the Rust
    /// states that hold a provision effect are reserved/active.
    pub fn delete_resource(&mut self, id: &str) -> Result<CatalogResource, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let resource = read_resource(&tx, id)?;
        let definitions: i64 = tx.query_row(
            "SELECT COUNT(*) FROM agent_definitions WHERE resource_id=?1",
            [id],
            |r| r.get(0),
        )?;
        // TS throws EngagementError('conflict', 'Remove unused Agent
        // definitions before deleting their resource') — both guards are 409.
        if definitions > 0 {
            return Err(Error::Conflict);
        }
        let live: i64 = tx.query_row(
            "SELECT COUNT(*) FROM engagements WHERE resource_id=?1 AND state IN ('reserved','active')",
            [id],
            |r| r.get(0),
        )?;
        if live > 0 {
            return Err(Error::Conflict);
        }
        tx.execute("DELETE FROM resources WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(resource.catalog())
    }

    /// TS DELETE /api/seats/:seatId: 404 when no declaration exists.
    pub fn delete_seat(&mut self, id: &str) -> Result<(), Error> {
        project::identifier(id, 128)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let declared = tx
            .query_row(
                "SELECT json_extract(config,'$.declaration') IS NOT NULL FROM seats WHERE id=?1",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .optional()?;
        if declared != Some(true) {
            return Err(Error::NotFound);
        }
        tx.execute("DELETE FROM seats WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// A definition requires a published, provisionable resource with a ceiling and
    /// a role the resource has qualified. Built-in resources derive that role set from
    /// the embedded model policy; plugin resources use their plugin qualification.
    fn definition_qualifies(resource: &Resource, role: &str) -> bool {
        resource.qualifies(role)
    }

    /// TS resourceAgentDefinitions.edit, ported with its exact rules and
    /// order. `None` input deletes; `Some` creates or updates.
    pub fn edit_agent_definition(
        &mut self,
        resource_id: &str,
        definition_id: Option<&str>,
        input: Option<&AgentDefinitionInput>,
        now: u64,
    ) -> Result<Option<AgentDefinition>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let resource = read_resource(&tx, resource_id)?;
        let previous: Vec<AgentDefinition> = {
            let mut query = tx.prepare(
                "SELECT id,resource_id,name,role,enabled,created_at,harness_template FROM agent_definitions \
                 WHERE resource_id=?1 ORDER BY created_at,id",
            )?;
            query
                .query_map([resource_id], |r| {
                    Ok(AgentDefinition {
                        id: r.get(0)?,
                        resource_id: r.get(1)?,
                        name: r.get(2)?,
                        role: r.get(3)?,
                        enabled: r.get::<_, i64>(4)? == 1,
                        created_at: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                        harness_template: r
                            .get::<_, Option<String>>(6)?
                            .map(|value| serde_json::from_str(&value))
                            .transpose()
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    6,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                    })
                })?
                .collect::<Result<_, _>>()?
        };
        let old = definition_id.and_then(|id| previous.iter().find(|d| d.id == id));
        if definition_id.is_some() && old.is_none() {
            return Err(Error::NotFound);
        }
        let in_use = |tx: &Transaction<'_>, name: &str| -> Result<bool, Error> {
            Ok(tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM engagements WHERE name=?1 \
                     AND state IN ('pending','reserved','active'))",
                    [name],
                    |r| r.get(0),
                )
                .unwrap_or(false))
        };
        let saved: Option<AgentDefinition> = match input {
            None => {
                let old = old.expect("checked above");
                if in_use(&tx, &old.name)? {
                    return Err(Error::Conflict);
                }
                tx.execute("DELETE FROM agent_definitions WHERE id=?1", [&old.id])?;
                // TS edit() returns `next.find(d => d.id === id) || next.at(-1)
                // || null` after a delete: the row is gone, so the answer is
                // the LAST REMAINING definition, never the deleted row.
                previous.iter().rfind(|d| d.id != old.id).cloned()
            }
            Some(input) => {
                let old = old.cloned();
                let name = input
                    .name
                    .clone()
                    .or_else(|| old.as_ref().map(|o| o.name.clone()))
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let role = input
                    .role
                    .clone()
                    .or_else(|| old.as_ref().map(|o| o.role.clone()))
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !valid_agent_name(&name) {
                    return Err(hagency_core::InvalidInput(
                        "Agent name must start with a lowercase letter and contain only lowercase letters, numbers, underscores or hyphens (64 characters maximum)",
                    )
                    .into());
                }
                if previous
                    .iter()
                    .any(|d| definition_id.is_none_or(|id| d.id != id) && d.name == name)
                {
                    return Err(Error::Conflict);
                }
                let template = match input.harness_template.as_ref() {
                    Some(value) => Some(validate_harness_template(&resource, value)?),
                    None => old
                        .as_ref()
                        .and_then(|definition| definition.harness_template.clone()),
                };
                if let Some(old) = &old
                    && in_use(&tx, &old.name)?
                    && (name != old.name || role != old.role || template != old.harness_template)
                {
                    return Err(Error::Conflict);
                }
                if !Self::definition_qualifies(&resource, &role) {
                    return Err(Error::Unqualified);
                }
                if old.is_none() && previous.len() >= 200 {
                    return Err(hagency_core::InvalidInput(
                        "This resource already has 200 Agent definitions",
                    )
                    .into());
                }
                let enabled = input
                    .enabled
                    .or_else(|| old.as_ref().map(|o| o.enabled))
                    .unwrap_or(true);
                let id = match &old {
                    Some(old) => old.id.clone(),
                    None => format!("rad_{}", random_hex()?),
                };
                let definition = AgentDefinition {
                    id,
                    resource_id: resource_id.to_owned(),
                    name,
                    role,
                    enabled,
                    created_at: old.as_ref().map(|o| o.created_at).unwrap_or(now),
                    harness_template: template,
                };
                let template = definition
                    .harness_template
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                if old.is_some() {
                    tx.execute(
                        "UPDATE agent_definitions SET name=?2,role=?3,enabled=?4,harness_template=?5 WHERE id=?1",
                        params![
                            definition.id,
                            definition.name,
                            definition.role,
                            definition.enabled,
                            template,
                        ],
                    )?;
                } else {
                    tx.execute(
                        "INSERT INTO agent_definitions(id,resource_id,name,role,enabled,created_at,harness_template) \
                         VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![definition.id, definition.resource_id, definition.name,
                            definition.role, definition.enabled, definition.created_at, template],
                    )?;
                }
                Some(definition)
            }
        };
        tx.commit()?;
        Ok(saved)
    }

    /// TS list(): definition rows with derived `status` and
    /// `activeEngagements` (provisioned = an active engagement runs the name;
    /// reserved = a live pending/reserved one holds it).
    pub fn agent_definitions(&self, resource_id: &str) -> Result<Vec<Value>, Error> {
        let mut query = self.db.prepare(
            "SELECT id,resource_id,name,role,enabled,created_at,harness_template FROM agent_definitions \
             WHERE resource_id=?1 ORDER BY created_at,id",
        )?;
        let rows = query
            .query_map([resource_id], |r| {
                Ok(AgentDefinition {
                    id: r.get(0)?,
                    resource_id: r.get(1)?,
                    name: r.get(2)?,
                    role: r.get(3)?,
                    enabled: r.get::<_, i64>(4)? == 1,
                    created_at: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                    harness_template: r
                        .get::<_, Option<String>>(6)?
                        .map(|value| serde_json::from_str(&value))
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|d| {
                let active: i64 = self.db.query_row(
                    "SELECT COUNT(*) FROM engagements WHERE name=?1 AND state='active'",
                    [&d.name],
                    |r| r.get(0),
                )?;
                let holding: i64 = self.db.query_row(
                    "SELECT COUNT(*) FROM engagements WHERE name=?1 AND state IN ('pending','reserved')",
                    [&d.name],
                    |r| r.get(0),
                )?;
                let status = if active > 0 {
                    "provisioned"
                } else if holding > 0 {
                    "reserved"
                } else {
                    "defined"
                };
                Ok(json!({"id":d.id,"name":d.name,"role":d.role,
                    "enabled":d.enabled,"createdAt":d.created_at,"status":status,
                    "activeEngagements":active,"harnessTemplate":d.harness_template}))
            })
            .collect()
    }
}

/// rad_ ids mirror the TS `rad_${randomUUID()}` shape: 32 hex characters.
fn random_hex() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| hagency_core::InvalidInput("definition id unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
