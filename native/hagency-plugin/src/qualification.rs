use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePluginRef {
    pub plugin_id: String,
    pub adapter_id: String,
    pub package_sha256: String,
    pub settings_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessPluginRef {
    pub plugin_id: String,
    pub harness_id: String,
    pub package_sha256: String,
    pub settings_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeQualification {
    pub runtime: RuntimePluginRef,
    pub framework: String,
    pub model: String,
    pub provider: String,
    pub reasoning: Option<String>,
    #[serde(default)]
    pub evidence_digest: String,
    #[serde(default)]
    pub roles: Vec<String>,
}
impl RuntimeQualification {
    pub fn matches(
        &self,
        runtime: &RuntimePluginRef,
        model: &str,
        provider: &str,
        reasoning: Option<&str>,
    ) -> bool {
        &self.runtime == runtime
            && self.model == model
            && self.provider == provider
            && self.reasoning.as_deref() == reasoning
    }
}
pub fn evidence_digest(transcript: &[u8]) -> String {
    let digest = Sha256::digest(transcript);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
