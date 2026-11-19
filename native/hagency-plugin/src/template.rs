use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const HARNESS_TEMPLATE_SCHEMA: &str = "hagency.harness.template/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessTemplateDocument {
    pub schema: String,
    pub plugin_id: String,
    pub harness_id: String,
    pub package_sha256: String,
    pub settings_sha256: String,
    pub template_sha256: String,
    pub values: BTreeMap<String, Value>,
}

pub fn digest(document: &HarnessTemplateDocument) -> Result<String, serde_json::Error> {
    let mut canonical = BTreeMap::new();
    canonical.insert("harnessId", Value::String(document.harness_id.clone()));
    canonical.insert(
        "packageSha256",
        Value::String(document.package_sha256.clone()),
    );
    canonical.insert("pluginId", Value::String(document.plugin_id.clone()));
    canonical.insert("schema", Value::String(document.schema.clone()));
    canonical.insert(
        "settingsSha256",
        Value::String(document.settings_sha256.clone()),
    );
    canonical.insert("values", serde_json::to_value(&document.values)?);
    let bytes = serde_json::to_vec(&canonical)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}
