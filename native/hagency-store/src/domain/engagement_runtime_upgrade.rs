//! ADR-197: engagements on a resource whose runtime package changed.
//!
//! An engagement freezes its resource, including the runtime package and
//! settings digest, in its provision effect. A resource-level runtime change
//! does not touch that frozen copy; it only records, per live engagement, that
//! a newer target exists (`upgrade_pending`). The engagement keeps running on
//! its frozen package until a later guarded rebind.
use super::DomainRepository;
use crate::Error;
use hagency_core::project::{Resource, RuntimePluginIdentity};
use rusqlite::{Connection, params};
use serde::Serialize;

/// Live engagement states: the ones whose frozen package must stay launchable.
const LIVE: &str = "('pending','reserved','active')";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeState {
    UpgradePending,
    Pinned,
    Halted,
}
impl UpgradeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpgradePending => "upgrade_pending",
            Self::Pinned => "pinned",
            Self::Halted => "halted",
        }
    }
    fn parse(word: &str) -> Result<Self, Error> {
        match word {
            "upgrade_pending" => Ok(Self::UpgradePending),
            "pinned" => Ok(Self::Pinned),
            "halted" => Ok(Self::Halted),
            _ => Err(Error::Schema),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngagementRuntimeUpgrade {
    pub engagement_id: String,
    pub resource_id: String,
    pub state: UpgradeState,
    /// The resource's current runtime identity the engagement may be rebound to.
    pub target_runtime: RuntimePluginIdentity,
    /// The runtime identity frozen in the engagement's provision effect.
    pub frozen_runtime: Option<RuntimePluginIdentity>,
    pub marked_at: u64,
}

/// True when `new` differs from `old` only in the runtime package or settings
/// digest of the same plugin and adapter. Anything else keeps the in-use guard.
pub(super) fn is_runtime_revision_change(old: &Resource, new: &Resource) -> bool {
    let (Some(a), Some(b)) = (&old.runtime_plugin, &new.runtime_plugin) else {
        return false;
    };
    a.plugin_id == b.plugin_id
        && a.adapter_id == b.adapter_id
        && a != b
        && old.seat_id == new.seat_id
        && old.framework == new.framework
        && old.model == new.model
        && old.provider == new.provider
        && old.reasoning == new.reasoning
        && old.octos_profile == new.octos_profile
        && old.harness_plugin == new.harness_plugin
}

/// Mark every reserved/active engagement on `resource` whose frozen runtime
/// differs from the resource's new runtime; clear the mark of one that already
/// matches. Runs in the caller's transaction.
pub(super) fn mark_pending(tx: &Connection, resource: &Resource, now: u64) -> Result<(), Error> {
    let Some(target) = &resource.runtime_plugin else {
        return Ok(());
    };
    let resource_id = resource.id();
    let target_json = serde_json::to_string(target)?;
    let mut statement = tx.prepare(
        "SELECT e.id,json_extract(f.payload,'$.resource.runtimePlugin') FROM engagements e \
         LEFT JOIN effects f ON f.engagement_id=e.id AND f.kind='provision' \
         WHERE e.resource_id=?1 AND e.state IN ('reserved','active')",
    )?;
    let rows = statement
        .query_map([&resource_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (engagement_id, frozen) in rows {
        let frozen: Option<RuntimePluginIdentity> =
            frozen.map(|v| serde_json::from_str(&v)).transpose()?;
        if frozen.as_ref() == Some(target) {
            tx.execute(
                "DELETE FROM engagement_runtime_upgrades WHERE engagement_id=?1",
                [&engagement_id],
            )?;
            continue;
        }
        // A pinned or halted engagement keeps its state; only the target moves.
        tx.execute(
            "INSERT INTO engagement_runtime_upgrades(engagement_id,resource_id,state,target_runtime,marked_at,updated_at) \
             VALUES(?1,?2,'upgrade_pending',?3,?4,?4) \
             ON CONFLICT(engagement_id) DO UPDATE SET target_runtime=excluded.target_runtime,updated_at=excluded.updated_at",
            params![engagement_id, resource_id, target_json, now],
        )?;
    }
    Ok(())
}

impl DomainRepository {
    /// Marked engagements, optionally of one resource. Ended engagements are
    /// not listed.
    pub fn engagement_runtime_upgrades(
        &self,
        resource_id: Option<&str>,
    ) -> Result<Vec<EngagementRuntimeUpgrade>, Error> {
        let mut statement = self.db.prepare(&format!(
            "SELECT u.engagement_id,u.resource_id,u.state,u.target_runtime,\
             json_extract(f.payload,'$.resource.runtimePlugin'),u.marked_at \
             FROM engagement_runtime_upgrades u JOIN engagements e ON e.id=u.engagement_id \
             LEFT JOIN effects f ON f.engagement_id=e.id AND f.kind='provision' \
             WHERE e.state IN {LIVE} AND (?1 IS NULL OR u.resource_id=?1) ORDER BY u.engagement_id"
        ))?;
        let rows = statement
            .query_map([resource_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(engagement_id, resource_id, state, target, frozen, marked_at)| {
                    Ok(EngagementRuntimeUpgrade {
                        engagement_id,
                        resource_id,
                        state: UpgradeState::parse(&state)?,
                        target_runtime: serde_json::from_str(&target)?,
                        frozen_runtime: frozen.map(|v| serde_json::from_str(&v)).transpose()?,
                        marked_at,
                    })
                },
            )
            .collect()
    }

    /// `(plugin_id, package_sha256, live engagements)` for the runtime package
    /// frozen in the provision effect of every live engagement. The plugin
    /// registry keeps these revisions launchable and never collects them.
    pub fn engagement_runtime_references(&self) -> Result<Vec<(String, String, u32)>, Error> {
        let mut statement = self.db.prepare(&format!(
            "SELECT json_extract(f.payload,'$.resource.runtimePlugin.pluginId'),\
             json_extract(f.payload,'$.resource.runtimePlugin.packageSha256'),COUNT(*) \
             FROM engagements e JOIN effects f ON f.engagement_id=e.id AND f.kind='provision' \
             WHERE e.state IN {LIVE} AND json_extract(f.payload,'$.resource.runtimePlugin.packageSha256') IS NOT NULL \
             GROUP BY 1,2 ORDER BY 1,2"
        ))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
