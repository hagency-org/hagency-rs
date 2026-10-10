//! The provider-owned local binding a Host runs with: the user's own Codex or
//! Claude Code sign-in folder (`LocalCodex`, ADR-192) or their Octos home
//! (`LocalOctos`, ADR-193). Each kind keeps its own checks; this only routes
//! the Host's calls to the one it holds.
use crate::{Failure, LocalCodex, LocalOctos, LocalProvider, Runner};
use hagency_store::{OwnedClaimProfile, OwnedDispatchScope, OwnedProvisionScope};
use std::{collections::BTreeMap, ffi::OsString};

pub(crate) enum LocalBinding {
    Folder(LocalCodex),
    Octos(LocalOctos),
}
impl LocalBinding {
    /// Whether this is the runner's own agent folder.
    pub(crate) fn serves(&self, runner: Runner) -> bool {
        match (self, runner) {
            (Self::Folder(local), Runner::Codex) => local.provider() == LocalProvider::Codex,
            (Self::Folder(local), Runner::Claude) => local.provider() == LocalProvider::Claude,
            (Self::Octos(_), Runner::Octos) => true,
            _ => false,
        }
    }
    pub(crate) fn check(&self) -> Result<(), Failure> {
        match self {
            Self::Folder(local) => local.check(),
            Self::Octos(local) => local.check(),
        }
    }
    pub(crate) fn bind_claim(
        &self,
        profile: OwnedClaimProfile,
    ) -> Result<OwnedClaimProfile, Failure> {
        match self {
            Self::Folder(local) => local.bind_claim(profile),
            Self::Octos(local) => local.bind_claim(profile),
        }
    }
    pub(crate) fn admit(&self, scope: &OwnedDispatchScope) -> Result<(), Failure> {
        match self {
            Self::Folder(local) => local.admit(scope),
            Self::Octos(local) => local.admit(scope),
        }
    }
    pub(crate) fn admit_provision(&self, scope: &OwnedProvisionScope) -> Result<(), Failure> {
        match self {
            Self::Folder(local) => local.admit_provision(scope),
            Self::Octos(local) => local.admit_provision(scope),
        }
    }
    pub(crate) fn separate_from(&self, workspace: &std::path::Path) -> Result<(), Failure> {
        match self {
            Self::Folder(local) => local.separate_from(workspace),
            Self::Octos(local) => local.separate_from(workspace),
        }
    }
    pub(crate) fn apply(
        &self,
        environment: &mut BTreeMap<OsString, OsString>,
    ) -> Result<(), Failure> {
        match self {
            Self::Folder(local) => local.apply(environment),
            Self::Octos(local) => local.apply(environment),
        }
    }
    /// The turn's future is boxed first: it is large, a dispatch thread
    /// holds the whole operation on its bounded stack, and this level would
    /// otherwise store another copy of it.
    pub(crate) fn watch<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, Failure>>,
    ) -> impl std::future::Future<Output = Result<T, Failure>> {
        let future = Box::pin(future);
        async move {
            match self {
                Self::Folder(local) => local.watch(future).await,
                Self::Octos(local) => local.watch(future).await,
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    /// Each binding serves its own runner only: a Codex folder never binds a
    /// Claude Code or Octos host, and an Octos home never binds the others.
    #[test]
    fn native_local_binding_serves_its_own_runner() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let folder = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        let home = folder("home");
        let codex = LocalBinding::Folder(
            LocalCodex::new("pool".into(), "seat".into(), home.clone(), folder("codex")).unwrap(),
        );
        let claude = LocalBinding::Folder(
            LocalCodex::new_claude("pool".into(), "seat".into(), home.clone(), folder("claude"))
                .unwrap(),
        );
        let octos = LocalBinding::Octos(
            LocalOctos::new(
                "pool".into(),
                "seat".into(),
                home,
                folder("octos"),
                vec!["coding".into()],
            )
            .unwrap(),
        );
        for (binding, runner) in [
            (&codex, Runner::Codex),
            (&claude, Runner::Claude),
            (&octos, Runner::Octos),
        ] {
            for other in [Runner::Codex, Runner::Claude, Runner::Octos] {
                assert_eq!(binding.serves(other), other == runner);
            }
        }
    }
}
