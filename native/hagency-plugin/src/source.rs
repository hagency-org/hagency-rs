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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitTransport {
    #[default]
    Https,
    Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PluginSource {
    Github {
        owner: String,
        repo: String,
        subdir: Option<String>,
        commit: String,
        #[serde(default)]
        transport: GitTransport,
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
        transport: GitTransport,
    ) -> Result<Self, SourceError> {
        Self::Github {
            owner: owner.into(),
            repo: repo.into(),
            subdir: subdir.map(str::to_owned),
            commit: commit.into(),
            transport,
        }
        .normalize()
    }
    pub fn linked(path: impl Into<PathBuf>) -> Result<Self, SourceError> {
        Self::Linked { path: path.into() }.normalize()
    }
    pub fn normalize(self) -> Result<Self, SourceError> {
        match self {
            Self::Github {
                owner,
                repo,
                subdir,
                commit,
                transport,
            } => {
                validate_segment(&owner, "owner")?;
                validate_segment(&repo, "repo")?;
                let subdir = subdir.map(|value| validate_subdir(&value)).transpose()?;
                validate_commit(&commit)?;
                let source = Self::Github {
                    owner,
                    repo,
                    subdir,
                    commit,
                    transport,
                };
                source.canonical_repository_url()?;
                Ok(source)
            }
            Self::Linked { path } => {
                validate_linked_root(&path)?;
                Ok(Self::Linked { path })
            }
        }
    }
    pub fn canonical_repository_url(&self) -> Result<String, SourceError> {
        let Self::Github {
            owner,
            repo,
            transport,
            ..
        } = self
        else {
            return Err(SourceError::Invalid(
                "linked sources do not have a repository URL".into(),
            ));
        };
        validate_segment(owner, "owner")?;
        validate_segment(repo, "repo")?;
        let url = match transport {
            GitTransport::Https => format!("https://github.com/{owner}/{repo}.git"),
            GitTransport::Ssh => format!("ssh://git@github.com/{owner}/{repo}.git"),
        };
        let parsed = Url::parse(&url)
            .map_err(|_| SourceError::Invalid("invalid GitHub repository URL".into()))?;
        let valid = match transport {
            GitTransport::Https => {
                parsed.scheme() == "https"
                    && parsed.host_str() == Some("github.com")
                    && parsed.username().is_empty()
                    && parsed.port().is_none()
            }
            GitTransport::Ssh => {
                parsed.scheme() == "ssh"
                    && parsed.host_str() == Some("github.com")
                    && parsed.username() == "git"
                    && parsed.port().is_none()
            }
        } && parsed.query().is_none()
            && parsed.fragment().is_none();
        if !valid {
            return Err(SourceError::Invalid(
                "GitHub source URL is not canonical".into(),
            ));
        }
        Ok(url)
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
                transport,
            } => format!(
                "{}{}@{commit}",
                match transport {
                    GitTransport::Https => format!("https://github.com/{owner}/{repo}"),
                    GitTransport::Ssh => format!("ssh://git@github.com/{owner}/{repo}"),
                },
                subdir
                    .as_deref()
                    .map(|value| format!("/{value}"))
                    .unwrap_or_default()
            ),
            Self::Linked { path } => format!("linked:{}", path.display()),
        }
    }
}

fn validate_linked_root(path: &Path) -> Result<(), SourceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(SourceError::UnsafeTree(path.display().to_string()));
    }
    if !path.is_absolute() {
        return Err(SourceError::Invalid("linked path must be absolute".into()));
    }
    if !metadata.is_dir() {
        return Err(SourceError::Invalid(
            "linked path must be a directory".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("git is unavailable")]
    GitUnavailable,
    #[error("git authentication is unavailable")]
    SshAuthUnavailable,
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
    let source = source.clone().normalize()?;
    let PluginSource::Github {
        subdir,
        commit,
        transport,
        ..
    } = &source
    else {
        return Err(SourceError::Invalid(
            "checkout requires a GitHub source".into(),
        ));
    };
    let url = source.canonical_repository_url()?;
    fs::create_dir_all(destination)?;
    let repo_dir = destination.join("repo");
    let home = destination.join("home");
    let config = destination.join("config");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&config)?;
    let global_config = config.join("global");
    fs::File::create(&global_config)?.sync_all()?;
    let ssh = if *transport == GitTransport::Ssh {
        Some(prepare_ssh(destination)?)
    } else {
        None
    };
    let run_git = |cwd: &Path, args: &[&str]| -> Result<std::process::Output, SourceError> {
        let mut command = std::process::Command::new("git");
        command.args([
            "-c",
            "protocol.file.allow=never",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "http.followRedirects=false",
        ]);
        command.args(args).current_dir(cwd).env_clear();
        command
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env("GIT_CONFIG_GLOBAL", &global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        if let Some((socket, ssh_command)) = &ssh {
            command
                .env("SSH_AUTH_SOCK", socket)
                .env("GIT_SSH_COMMAND", ssh_command);
        }
        command.output().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SourceError::GitUnavailable
            } else {
                SourceError::Io(error)
            }
        })
    };
    let repo_text = repo_dir
        .to_str()
        .ok_or_else(|| SourceError::Invalid("non-UTF-8 checkout path".into()))?;
    ensure_git_success(run_git(destination, &["init", "--quiet", repo_text])?)?;
    let run_checked = |args: &[&str]| -> Result<(), SourceError> {
        ensure_git_success(run_git(&repo_dir, args)?)
    };
    run_checked(&["remote", "add", "origin", &url])?;
    run_checked(&[
        "fetch",
        "--depth",
        "1",
        "--no-tags",
        "--no-recurse-submodules",
        "origin",
        commit,
    ])?;
    run_checked(&["checkout", "--detach", "--quiet", commit])?;
    let head = run_git(&repo_dir, &["rev-parse", "HEAD"])?;
    ensure_git_success(head.clone())?;
    let observed = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if observed != commit.as_str() {
        return Err(SourceError::Git(
            "checked out HEAD does not match the requested commit".into(),
        ));
    }
    if fs::symlink_metadata(repo_dir.join(".gitmodules")).is_ok() {
        return Err(SourceError::UnsafeTree(".gitmodules".into()));
    }
    fs::remove_dir_all(repo_dir.join(".git"))?;
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

fn ensure_git_success(output: std::process::Output) -> Result<(), SourceError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(SourceError::Git(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn prepare_ssh(destination: &Path) -> Result<(std::ffi::OsString, String), SourceError> {
    #[cfg(not(unix))]
    {
        let _ = destination;
        return Err(SourceError::SshAuthUnavailable);
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::{fs::FileTypeExt, fs::PermissionsExt, net::UnixStream};
        use std::time::Duration;
        let socket = std::env::var_os("SSH_AUTH_SOCK").ok_or(SourceError::SshAuthUnavailable)?;
        let socket_path = PathBuf::from(&socket);
        let metadata =
            fs::symlink_metadata(&socket_path).map_err(|_| SourceError::SshAuthUnavailable)?;
        if !metadata.file_type().is_socket() {
            return Err(SourceError::SshAuthUnavailable);
        }
        let stream =
            UnixStream::connect(&socket_path).map_err(|_| SourceError::SshAuthUnavailable)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|_| SourceError::SshAuthUnavailable)?;
        stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .map_err(|_| SourceError::SshAuthUnavailable)?;
        drop(stream);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(SourceError::SshAuthUnavailable)?;
        let known_hosts = home.join(".ssh/known_hosts");
        let metadata =
            fs::symlink_metadata(&known_hosts).map_err(|_| SourceError::SshAuthUnavailable)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SourceError::SshAuthUnavailable);
        }
        let mut input =
            fs::File::open(&known_hosts).map_err(|_| SourceError::SshAuthUnavailable)?;
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|_| SourceError::SshAuthUnavailable)?;
        let staged = destination.join("home/.ssh/known_hosts");
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent).map_err(|_| SourceError::SshAuthUnavailable)?;
        }
        let mut output = fs::File::create(&staged).map_err(|_| SourceError::SshAuthUnavailable)?;
        use std::io::Write;
        output
            .write_all(&bytes)
            .and_then(|_| output.sync_all())
            .map_err(|_| SourceError::SshAuthUnavailable)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))
            .map_err(|_| SourceError::SshAuthUnavailable)?;
        let staged = staged
            .to_str()
            .ok_or(SourceError::SshAuthUnavailable)?
            .replace('\'', "'\\''");
        Ok((
            socket,
            format!(
                "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile='{}'",
                staged
            ),
        ))
    }
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
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
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
    Ok(value.into())
}
