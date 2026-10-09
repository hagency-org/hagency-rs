//! The user's own Octos home as a provider-owned local binding (ADR-193
//! decision 7). It is kept apart from the Codex and Claude Code binding
//! (`local_codex.rs`), whose source is part of the qualified Claude launch
//! path (ADR-192 slice 4); the folder checks are the same.
use crate::{AuthoritySite, Failure};
use cap_std::{ambient_authority, fs::Dir};
use hagency_store::{OwnedClaimProfile, OwnedDispatchScope};
use std::{collections::BTreeMap, ffi::OsString, fs::File, path::PathBuf};

/// The most profiles one Octos binding may allow.
const MAX_PROFILES: usize = 64;

/// A folder this binding names, held open: the same checks as the local
/// Codex binding's (canonical path, same directory, this user's, not group
/// or world writable).
struct Directory {
    path: PathBuf,
    file: File,
}
impl Directory {
    fn open(path: PathBuf) -> Result<Self, Failure> {
        if !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().len() > 4096
            || path.canonicalize().ok().as_ref() != Some(&path)
        {
            return Err(Failure::Admission);
        }
        let file = Dir::open_ambient_dir(&path, ambient_authority())
            .map_err(|_| Failure::Admission)?
            .into_std_file();
        let value = Self { path, file };
        value.check()?;
        Ok(value)
    }
    fn check(&self) -> Result<(), Failure> {
        let lost = || Failure::lost_io(AuthoritySite::LocalOctosCheck);
        if self.path.canonicalize().ok().as_ref() != Some(&self.path) {
            return Err(lost());
        }
        let current = Dir::open_ambient_dir(&self.path, ambient_authority())
            .map_err(|_| lost())?
            .into_std_file();
        if !hagency_platform::same_directory(&self.file, &current).map_err(|_| lost())? {
            return Err(lost());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = self.file.metadata().map_err(|_| lost())?;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0
            {
                return Err(lost());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(Failure::Admission)
        }
    }
}

/// The user's Octos home and the profiles the machine owner lets Hagency run
/// from it. Host-only explicit selection; never an authentication fact or a
/// wire grant. Octos keeps its keys itself: the binding reads only each
/// profile's primary model, at admission.
pub struct LocalOctos {
    preset: String,
    seat: String,
    home: Directory,
    octos: Directory,
    profiles: Vec<String>,
    /// The service's search path, OS user name and temporary folder: with
    /// HOME, the only environment Octos gets (ADR-193 decision 3).
    path: Option<OsString>,
    user: Option<OsString>,
    tmpdir: Option<OsString>,
}
impl LocalOctos {
    pub fn new(
        preset: String,
        seat: String,
        home: PathBuf,
        octos_home: PathBuf,
        profiles: Vec<String>,
    ) -> Result<Self, Failure> {
        for id in [&preset, &seat] {
            hagency_core::project::identifier(id, 128).map_err(|_| Failure::Admission)?;
        }
        let mut unique = std::collections::BTreeSet::new();
        if profiles.is_empty()
            || profiles.len() > MAX_PROFILES
            || !profiles.iter().all(|profile| {
                hagency_runtime::octos::profile_id(profile) && unique.insert(profile.as_str())
            })
        {
            return Err(Failure::Admission);
        }
        let value = Self {
            preset,
            seat,
            home: Directory::open(home)?,
            octos: Directory::open(octos_home)?,
            profiles,
            path: std::env::var_os("PATH"),
            user: std::env::var_os("USER"),
            tmpdir: std::env::var_os("TMPDIR"),
        };
        value.check()?;
        Ok(value)
    }
    pub(crate) fn check(&self) -> Result<(), Failure> {
        self.home.check()?;
        self.octos.check()
    }
    /// The claim is narrowed to the binding's seat, as for Codex and Claude
    /// Code; every allowed profile's resource is on it.
    pub(crate) fn bind_claim(
        &self,
        profile: OwnedClaimProfile,
    ) -> Result<OwnedClaimProfile, Failure> {
        self.check()?;
        profile
            .restrict_resource(self.preset.clone(), self.seat.clone())
            .map_err(|_| Failure::Admission)
    }
    pub(crate) fn admit(&self, scope: &OwnedDispatchScope) -> Result<(), Failure> {
        self.admit_resource(scope.resource(), scope.requires_managed_account())
    }
    pub(crate) fn admit_provision(
        &self,
        scope: &hagency_store::OwnedProvisionScope,
    ) -> Result<(), Failure> {
        self.admit_resource(scope.resource(), scope.requires_managed_account())
    }
    /// ADR-193 decision 7: an Octos resource on this seat that runs a profile
    /// the machine owner allowed, whose primary is still the resource's
    /// provider family and model. A profile the user changed refuses the
    /// dispatch instead of silently running another model.
    fn admit_resource(
        &self,
        resource: &hagency_core::project::Resource,
        managed: bool,
    ) -> Result<(), Failure> {
        self.check()?;
        let profile = resource
            .octos_profile
            .as_deref()
            .ok_or(Failure::Admission)?;
        if managed
            || resource.seat_id != self.seat
            || resource.framework != "octos"
            || resource.reasoning.is_some()
            || !self.profiles.iter().any(|allowed| allowed == profile)
        {
            return Err(Failure::Admission);
        }
        let model = self.profile_model(profile).ok_or(Failure::Admission)?;
        if resource.provider.as_deref() != Some(model.family.as_str())
            || resource.model != model.model
        {
            return Err(Failure::Admission);
        }
        Ok(())
    }
    /// `profiles/<id>.json` read through the retained home, bounded; only its
    /// ID and primary are kept.
    fn profile_model(&self, profile: &str) -> Option<hagency_runtime::octos::ProfileModel> {
        use std::io::Read;
        if !hagency_runtime::octos::profile_id(profile) {
            return None;
        }
        let home = Dir::from_std_file(self.octos.file.try_clone().ok()?);
        let file = home.open(format!("profiles/{profile}.json")).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > hagency_runtime::octos::MAX_PROFILE_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(hagency_runtime::octos::MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        hagency_runtime::octos::profile_model(profile, &bytes)
    }
    pub(crate) fn separate_from(&self, workspace: &std::path::Path) -> Result<(), Failure> {
        if self.home.path.starts_with(workspace) || self.octos.path.starts_with(workspace) {
            return Err(Failure::Admission);
        }
        Ok(())
    }
    /// The environment ADR-193 decision 3 allows: HOME, USER, PATH and TMPDIR,
    /// and OCTOS_HOME only for a home other than `~/.octos`. Octos treats a
    /// named home as an explicit install with its own config location
    /// (`config_context.rs` at 41ad4911e), so the default is never named.
    pub(crate) fn apply(
        &self,
        environment: &mut BTreeMap<OsString, OsString>,
    ) -> Result<(), Failure> {
        self.check()?;
        if environment.keys().any(|key| {
            matches!(
                key.to_string_lossy().to_ascii_uppercase().as_str(),
                "OPENAI_API_KEY"
                    | "CODEX_API_KEY"
                    | "OPENAI_BASE_URL"
                    | "AZURE_OPENAI_API_KEY"
                    | "ANTHROPIC_API_KEY"
                    | "ANTHROPIC_AUTH_TOKEN"
                    | "ANTHROPIC_BASE_URL"
                    | "CLAUDE_CODE_OAUTH_TOKEN"
                    | "API_TOKEN"
                    | "MATRIX_BRIDGE_SECRET"
                    | "HAGENCY_DASHBOARD_TOKEN"
                    | "HAGENCY_SUBCONSCIOUS_EVENT_TOKEN"
                    | "MATRIX_BOT_PASSWORD"
                    | "MATRIX_REG_TOKEN"
                    | "MATRIX_AGENT_PASSWORD_SECRET"
            )
        }) {
            return Err(Failure::Admission);
        }
        environment.insert("HOME".into(), self.home.path.clone().into_os_string());
        if self.home.path.join(".octos").canonicalize().ok().as_ref() == Some(&self.octos.path) {
            environment.remove(&OsString::from("OCTOS_HOME"));
        } else {
            environment.insert(
                "OCTOS_HOME".into(),
                self.octos.path.clone().into_os_string(),
            );
        }
        for (name, value) in [
            ("PATH", &self.path),
            ("USER", &self.user),
            ("TMPDIR", &self.tmpdir),
        ] {
            if let Some(value) = value {
                environment.insert(name.into(), value.clone());
            }
        }
        Ok(())
    }
    pub(crate) async fn watch<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        tokio::pin!(future);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _=tick.tick()=>self.check()?,
                result=&mut future=>{self.check()?;return result;}
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    /// ADR-193 decisions 3 and 7: the user's Octos home and the profiles the
    /// machine owner allowed. The environment is the allowlist only; a
    /// dispatch runs only a resource whose allowed profile still names the
    /// resource's provider family and model, read through the retained home.
    #[test]
    fn native_local_octos_binding() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let home = root.join("home");
        let octos = home.join(".octos");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir_all(octos.join("profiles")).unwrap();
        std::fs::set_permissions(&octos, std::fs::Permissions::from_mode(0o700)).unwrap();
        let write_profile = |id: &str, family: &str, model: &str| {
            std::fs::write(
                octos.join("profiles").join(format!("{id}.json")),
                serde_json::to_vec(&serde_json::json!({"id": id, "name": id,
                    "config": {"llm": {"primary": {"family_id": family, "model_id": model}},
                        "env_vars": {"SYNTHETIC_API_KEY": "synthetic-forbidden"}}}))
                .unwrap(),
            )
            .unwrap();
        };
        write_profile("coding", "moonshot", "kimi-k3");
        write_profile("other", "moonshot", "kimi-k3");
        let binding = LocalOctos::new(
            "pool".into(),
            "seat".into(),
            home.clone(),
            octos.clone(),
            vec!["coding".into()],
        )
        .unwrap();
        let mut environment = BTreeMap::new();
        binding.apply(&mut environment).unwrap();
        assert_eq!(
            environment.get(&OsString::from("HOME")),
            Some(&home.clone().into_os_string())
        );
        // The default home is found by Octos itself, never named.
        assert!(!environment.contains_key(&OsString::from("OCTOS_HOME")));
        assert!(
            environment
                .keys()
                .all(|key| matches!(key.to_str(), Some("HOME" | "PATH" | "USER" | "TMPDIR")))
        );
        assert_eq!(
            environment.get(&OsString::from("TMPDIR")),
            std::env::var_os("TMPDIR").as_ref()
        );
        for key in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
            let mut leaked = environment.clone();
            leaked.insert(key.into(), "synthetic-forbidden".into());
            assert!(binding.apply(&mut leaked).is_err(), "{key} refused");
        }
        // Another home is named, as Octos's own OCTOS_HOME.
        let elsewhere = root.join("octos-elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        let named = LocalOctos::new(
            "pool".into(),
            "seat".into(),
            home.clone(),
            elsewhere.clone(),
            vec!["coding".into()],
        )
        .unwrap();
        let mut environment = BTreeMap::new();
        named.apply(&mut environment).unwrap();
        assert_eq!(
            environment.get(&OsString::from("OCTOS_HOME")),
            Some(&elsewhere.into_os_string())
        );
        // The allowed profiles are a non-empty set of profile IDs, and the
        // home must be canonical.
        for profiles in [
            vec![],
            vec!["../coding".to_owned()],
            vec!["coding".to_owned(), "coding".to_owned()],
        ] {
            assert!(
                LocalOctos::new(
                    "pool".into(),
                    "seat".into(),
                    home.clone(),
                    octos.clone(),
                    profiles
                )
                .is_err()
            );
        }
        let alias = root.join("alias");
        symlink(&octos, &alias).unwrap();
        assert!(
            LocalOctos::new(
                "pool".into(),
                "seat".into(),
                home.clone(),
                alias,
                vec!["coding".into()]
            )
            .is_err()
        );

        let resource = |profile: &str, provider: &str, model: &str| {
            serde_json::from_value::<hagency_core::project::Resource>(serde_json::json!({
                "presetId": "pool_coding", "seatId": "seat", "framework": "octos",
                "model": model, "provider": provider, "reasoning": null,
                "octosProfile": profile, "ceiling": {"tokens": 1000, "period": "monthly"}}))
            .unwrap()
        };
        let current = resource("coding", "moonshot", "kimi-k3");
        assert_eq!(binding.admit_resource(&current, false), Ok(()));
        // Managed accounts, other seats and other frameworks are refused.
        assert!(binding.admit_resource(&current, true).is_err());
        let mut other_seat = current.clone();
        other_seat.seat_id = "other_seat".into();
        assert!(binding.admit_resource(&other_seat, false).is_err());
        let mut codex = current.clone();
        codex.framework = "codex".into();
        assert!(binding.admit_resource(&codex, false).is_err());
        let mut reasoning = current.clone();
        reasoning.reasoning = Some("high".into());
        assert!(binding.admit_resource(&reasoning, false).is_err());
        let mut unnamed = current.clone();
        unnamed.octos_profile = None;
        assert!(binding.admit_resource(&unnamed, false).is_err());
        // A profile the machine owner did not allow, even with a matching primary.
        assert!(
            binding
                .admit_resource(&resource("other", "moonshot", "kimi-k3"), false)
                .is_err()
        );
        // The resource's model and family must be the profile's primary.
        assert!(
            binding
                .admit_resource(&resource("coding", "deepseek", "kimi-k3"), false)
                .is_err()
        );
        assert!(
            binding
                .admit_resource(&resource("coding", "moonshot", "kimi-k2"), false)
                .is_err()
        );
        // The user changed the profile: the next dispatch is refused, not
        // silently run on another model.
        write_profile("coding", "deepseek", "deepseek-v-flash");
        assert!(binding.admit_resource(&current, false).is_err());
        write_profile("coding", "moonshot", "kimi-k3");
        assert_eq!(binding.admit_resource(&current, false), Ok(()));
        // A profile file that leaves the retained home is never read.
        let outside = root.join("outside.json");
        std::fs::rename(octos.join("profiles/coding.json"), &outside).unwrap();
        symlink(&outside, octos.join("profiles/coding.json")).unwrap();
        assert!(binding.admit_resource(&current, false).is_err());
        std::fs::remove_file(octos.join("profiles/coding.json")).unwrap();
        assert!(binding.admit_resource(&current, false).is_err());
        // The home itself is checked like every local folder.
        std::fs::set_permissions(&octos, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            binding.check(),
            Err(Failure::lost_io(AuthoritySite::LocalOctosCheck))
        );
        std::fs::set_permissions(&octos, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(binding.check(), Ok(()));
    }
}
