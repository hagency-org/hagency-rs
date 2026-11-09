use crate::manifest::{MANIFEST_FILE, Manifest, ManifestError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;
use url::Url;

pub const COMMIT_LENGTH: usize = 40;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PluginSource {
    Github {
        owner: String,
        repo: String,
        subdir: Option<String>,
        commit: String,
    },
    Linked {
        path: PathBuf,
    },
}
impl PluginSource {
    pub fn github(
        owner: &str,
        repo: &str,
        subdir: Option<&str>,
        commit: &str,
    ) -> Result<Self, SourceError> {
        validate_segment(owner, "owner")?;
        validate_segment(repo, "repo")?;
        let subdir = subdir.map(validate_subdir).transpose()?;
        validate_commit(commit)?;
        Ok(Self::Github {
            owner: owner.into(),
            repo: repo.into(),
            subdir,
            commit: commit.into(),
        })
    }
    pub fn linked(path: impl Into<PathBuf>) -> Result<Self, SourceError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(SourceError::Invalid("linked path must be absolute".into()));
        }
        if !path.is_dir() {
            return Err(SourceError::Invalid(
                "linked path must be a directory".into(),
            ));
        }
        Ok(Self::Linked { path })
    }
    pub fn commit(&self) -> Option<&str> {
        match self {
            Self::Github { commit, .. } => Some(commit),
            Self::Linked { .. } => None,
        }
    }
    pub fn display(&self) -> String {
        match self {
            Self::Github {
                owner,
                repo,
                subdir,
                commit,
            } => format!(
                "{owner}/{repo}{}@{commit}",
                subdir
                    .as_deref()
                    .map(|v| format!("/{v}"))
                    .unwrap_or_default()
            ),
            Self::Linked { path } => format!("linked:{}", path.display()),
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("git is unavailable")]
    GitUnavailable,
    #[error("git checkout failed: {0}")]
    Git(String),
    #[error("invalid plugin source: {0}")]
    Invalid(String),
    #[error("plugin source contains a symlink or unsupported entry: {0}")]
    UnsafeTree(String),
    #[error("plugin source IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("plugin manifest failed: {0}")]
    Manifest(#[from] ManifestError),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub kind: String,
    pub mode: u32,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    pub digest: String,
}

pub fn validate_commit(commit: &str) -> Result<(), SourceError> {
    if commit.len() != COMMIT_LENGTH
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(SourceError::Invalid(
            "commit must be exactly 40 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

pub fn snapshot_tree(root: &Path) -> Result<SourceSnapshot, SourceError> {
    if !root.is_absolute() || !root.is_dir() {
        return Err(SourceError::Invalid(
            "source root must be an absolute directory".into(),
        ));
    }
    let mut files = Vec::new();
    collect_tree(root, root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Sha256::new();
    for file in &files {
        hash_field(&mut hasher, file.path.as_bytes());
        hash_field(&mut hasher, file.kind.as_bytes());
        hasher.update(file.mode.to_le_bytes());
        hasher.update(file.bytes.to_le_bytes());
        if file.kind == "file" {
            let bytes = fs::read(root.join(&file.path))?;
            hash_field(&mut hasher, &bytes);
        }
    }
    Ok(SourceSnapshot {
        root: root.to_path_buf(),
        files,
        digest: hex(&hasher.finalize()),
    })
}

pub fn package_manifest(root: &Path) -> Result<(Manifest, Vec<u8>, SourceSnapshot), SourceError> {
    let manifest_path = root.join(MANIFEST_FILE);
    let metadata = fs::symlink_metadata(&manifest_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SourceError::UnsafeTree(MANIFEST_FILE.into()));
    }
    let bytes = fs::read(&manifest_path)?;
    let manifest = Manifest::parse(&bytes)?;
    manifest.validate_package_root(root)?;
    let snapshot = snapshot_tree(root)?;
    Ok((manifest, bytes, snapshot))
}

pub fn checkout_github(source: &PluginSource, destination: &Path) -> Result<PathBuf, SourceError> {
    let PluginSource::Github {
        owner,
        repo,
        subdir,
        commit,
    } = source
    else {
        return Err(SourceError::Invalid(
            "checkout requires a GitHub source".into(),
        ));
    };
    validate_commit(commit)?;
    fs::create_dir_all(destination)?;
    let repo_dir = destination.join("repo");
    let url = format!("https://github.com/{owner}/{repo}.git");
    // Parsing the constructed URL prevents accidental alternate schemes/hosts if
    // validation is ever relaxed; git still receives a fixed argv below.
    let parsed = Url::parse(&url).map_err(|_| SourceError::Invalid("invalid GitHub URL".into()))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        return Err(SourceError::Invalid(
            "GitHub source must use https://github.com".into(),
        ));
    }
    let mut command = std::process::Command::new("git");
    command.args([
        "-c",
        "protocol.file.allow=never",
        "-c",
        "core.hooksPath=/dev/null",
        "init",
        "--quiet",
        repo_dir
            .to_str()
            .ok_or_else(|| SourceError::Invalid("non-UTF-8 checkout path".into()))?,
    ]);
    command.env_clear();
    command.env("HOME", destination.join("home"));
    command.env("XDG_CONFIG_HOME", destination.join("config"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let output = command.output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            SourceError::GitUnavailable
        } else {
            SourceError::Io(error)
        }
    })?;
    if !output.status.success() {
        return Err(SourceError::Git(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let run_git = |args: &[&str]| -> Result<(), SourceError> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .env_clear()
            .env("HOME", destination.join("home"))
            .env("XDG_CONFIG_HOME", destination.join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output()
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    SourceError::GitUnavailable
                } else {
                    SourceError::Io(error)
                }
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SourceError::Git(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    };
    run_git(&["remote", "add", "origin", &url])?;
    run_git(&[
        "fetch",
        "--depth",
        "1",
        "--no-tags",
        "--no-recurse-submodules",
        "origin",
        commit,
    ])?;
    run_git(&["checkout", "--detach", "--quiet", commit])?;
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repo_dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map_err(SourceError::Io)?;
    let observed = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if observed != commit.as_str() {
        return Err(SourceError::Git(
            "checked out HEAD does not match the requested commit".into(),
        ));
    }
    if repo_dir.join(".gitmodules").exists() {
        return Err(SourceError::UnsafeTree(".gitmodules".into()));
    }
    let root = if let Some(subdir) = subdir {
        repo_dir.join(subdir)
    } else {
        repo_dir
    };
    if !root.is_dir() {
        return Err(SourceError::Invalid(
            "GitHub subdirectory does not exist".into(),
        ));
    }
    snapshot_tree(&root)?;
    Ok(root)
}

fn collect_tree(
    root: &Path,
    current: &Path,
    files: &mut Vec<FileEntry>,
) -> Result<(), SourceError> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| SourceError::UnsafeTree(path.display().to_string()))?;
        let relative_text = relative
            .to_str()
            .ok_or_else(|| SourceError::UnsafeTree(relative.display().to_string()))?
            .replace('\\', "/");
        if relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(SourceError::UnsafeTree(relative_text));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(SourceError::UnsafeTree(relative_text));
        }
        let mode = file_mode(&metadata);
        if metadata.is_dir() {
            if relative_text == ".git" || relative_text.ends_with("/.git") {
                return Err(SourceError::UnsafeTree(relative_text));
            }
            files.push(FileEntry {
                path: relative_text.clone(),
                kind: "directory".into(),
                mode,
                bytes: 0,
            });
            collect_tree(root, &path, files)?;
        } else if metadata.is_file() {
            if metadata.len() > MAX_FILE_BYTES {
                return Err(SourceError::Invalid(format!(
                    "file {relative_text} is too large"
                )));
            }
            files.push(FileEntry {
                path: relative_text,
                kind: "file".into(),
                mode,
                bytes: metadata.len(),
            });
        } else {
            return Err(SourceError::UnsafeTree(relative_text));
        }
    }
    Ok(())
}
fn file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }
    #[cfg(not(unix))]
    {
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}
fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn validate_segment(value: &str, field: &str) -> Result<(), SourceError> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(SourceError::Invalid(format!("{field} is invalid")));
    }
    Ok(())
}
fn validate_subdir(value: &str) -> Result<String, SourceError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(SourceError::Invalid(
            "subdir must stay below the repository root".into(),
        ));
    }
    Ok(value.trim_matches('/').into())
}
