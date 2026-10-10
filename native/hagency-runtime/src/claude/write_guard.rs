//! Claude Code's file tools ask the owner before writing outside the
//! workspace. Claude Code 2.1.296 in `auto` mode writes anywhere on its own;
//! this `PreToolUse` hook answers `ask` for any Write, Edit, MultiEdit or
//! NotebookEdit whose target is not inside the dispatch's workspace, so the
//! request becomes an owner card like any other `can_use_tool`. A target it
//! cannot place is outside: the hook fails closed.
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

/// The file tools the hook watches.
pub const MATCHER: &str = "Write|Edit|MultiEdit|NotebookEdit";
/// The hook subcommand of the guard executable.
pub const COMMAND: &str = "claude-write-guard";
/// The most hook input read (a Write carries the whole file).
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// The `hooks` settings for one launch: the guard, run as
/// `'<executable>' claude-write-guard '<workspace>'`. `None` when a path
/// cannot be quoted for the shell Claude Code runs hooks with.
pub fn hooks(executable: &str, workspace: &str) -> Option<Value> {
    let quote = |path: &str| {
        (path.starts_with('/') && !path.contains(['\'', '\0', '\n', '\r']))
            .then(|| format!("'{path}'"))
    };
    let command = format!("{} {COMMAND} {}", quote(executable)?, quote(workspace)?);
    Some(json!({"PreToolUse":[{"matcher":MATCHER,
        "hooks":[{"type":"command","command":command,"timeout":10}]}]}))
}

/// Add the guard to a task launch (`task_arguments`): its one `--settings`
/// value gains the hook, and the hooks it disabled are enabled for this hook
/// alone (`--setting-sources=` already loads no other settings file).
pub fn guard(arguments: &mut [String], executable: &str, workspace: &str) -> Option<()> {
    let hooks = hooks(executable, workspace)?;
    let mut settings = arguments
        .iter_mut()
        .filter(|argument| argument.starts_with("--settings="));
    let argument = settings.next()?;
    if settings.next().is_some() {
        return None;
    }
    let mut value: Value = serde_json::from_str(argument.strip_prefix("--settings=")?).ok()?;
    value["disableAllHooks"] = false.into();
    value["hooks"] = hooks;
    *argument = format!("--settings={value}");
    Some(())
}
/// The hook's answer to one `PreToolUse` input: `Some` asks the owner, `None`
/// lets Claude Code decide as usual (the target is inside the workspace).
pub fn decide(input: &[u8], workspace: &Path) -> Option<Value> {
    let ask = |reason: &str| {
        Some(json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
            "permissionDecision":"ask","permissionDecisionReason":reason}}))
    };
    let outside = "This file is outside the agent's workspace: the owner decides.";
    let Ok(input) = serde_json::from_slice::<Value>(input) else {
        return ask(outside);
    };
    let tool = &input["tool_input"];
    let Some(target) = tool["file_path"]
        .as_str()
        .or_else(|| tool["notebook_path"].as_str())
    else {
        return ask(outside);
    };
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        match input["cwd"].as_str() {
            Some(cwd) if Path::new(cwd).is_absolute() => Path::new(cwd).join(target),
            _ => return ask(outside),
        }
    };
    match (resolve(&target), workspace.canonicalize()) {
        (Some(target), Ok(root)) if target.starts_with(&root) => None,
        _ => ask(outside),
    }
}

/// The target as the file system will see it: `.` and `..` folded, and the
/// deepest existing ancestor's links resolved, so neither a `..` nor a link
/// out of the workspace passes. The rest of the path does not exist yet.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => lexical.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !lexical.pop() {
                    return None;
                }
            }
            Component::Normal(part) => lexical.push(part),
            Component::Prefix(_) => return None,
        }
    }
    let mut existing = lexical.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut resolved = real;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
        rest.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

/// The guard executable's whole run: the hook input on stdin, the decision on
/// stdout, exit 0. Anything it cannot read asks.
pub fn run(workspace: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut input)?;
    if input.len() > MAX_INPUT_BYTES {
        input.clear();
    }
    if let Some(answer) = decide(&input, workspace) {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &answer)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(tool: &str, path: &str, cwd: &str) -> Vec<u8> {
        let key = if tool == "NotebookEdit" {
            "notebook_path"
        } else {
            "file_path"
        };
        serde_json::to_vec(&json!({"hook_event_name":"PreToolUse","tool_name":tool,
            "cwd":cwd,"tool_input":{key:path,"content":"x"}}))
        .unwrap()
    }
    fn asks(answer: Option<Value>) -> bool {
        answer.is_some_and(|a| a["hookSpecificOutput"]["permissionDecision"] == "ask")
    }
    #[test]
    fn native_claude_write_guard_asks_outside_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        let w = work.to_str().unwrap();
        // Inside: Claude decides as usual.
        assert!(decide(&input("Write", &format!("{w}/a.txt"), w), &work).is_none());
        assert!(decide(&input("Edit", &format!("{w}/src/new/b.rs"), w), &work).is_none());
        assert!(decide(&input("Write", "src/c.rs", w), &work).is_none());
        assert!(decide(&input("NotebookEdit", &format!("{w}/n.ipynb"), w), &work).is_none());
        // Outside, by path, by `..`, by a relative path from outside, or by a
        // link out of the workspace: the owner decides.
        let outside = root.join("outside.txt");
        assert!(asks(decide(
            &input("Write", outside.to_str().unwrap(), w),
            &work
        )));
        assert!(asks(decide(
            &input("MultiEdit", &format!("{w}/../outside.txt"), w),
            &work
        )));
        assert!(asks(decide(
            &input("Write", "x.txt", root.to_str().unwrap()),
            &work
        )));
        assert!(asks(decide(&input("Write", "/", w), &work)));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, work.join("link")).unwrap();
            assert!(asks(decide(
                &input("Write", &format!("{w}/link/outside.txt"), w),
                &work
            )));
        }
        // A workspace that is a prefix of a sibling's name is not that sibling.
        std::fs::create_dir(root.join("work2")).unwrap();
        let sibling = root.join("work2/x");
        assert!(asks(decide(
            &input("Write", sibling.to_str().unwrap(), w),
            &work
        )));
        // Anything it cannot read asks.
        assert!(asks(decide(b"not json", &work)));
        assert!(asks(decide(br#"{"tool_input":{}}"#, &work)));
        assert!(asks(decide(&input("Write", "rel.txt", "relative"), &work)));
    }
    #[test]
    fn native_claude_write_guard_joins_the_task_settings() {
        let mut args = crate::claude::task_arguments("sonnet", true).unwrap();
        guard(&mut args, "/opt/hagency", "/work").unwrap();
        let settings: Vec<_> = args
            .iter()
            .filter(|a| a.starts_with("--settings="))
            .collect();
        assert_eq!(settings.len(), 1);
        let value: Value = serde_json::from_str(&settings[0]["--settings=".len()..]).unwrap();
        assert_eq!(value["disableAllHooks"], false);
        assert_eq!(
            value["permissions"]["ask"],
            json!(["Bash(gh *)", "Bash(git push *)"])
        );
        assert_eq!(value["hooks"]["PreToolUse"][0]["matcher"], MATCHER);
        assert!(args.contains(&"--setting-sources=".to_owned()));
        assert!(guard(&mut ["--model=x".to_owned()], "/opt/hagency", "/work").is_none());
    }
    #[test]
    fn native_claude_write_guard_hook_settings() {
        let settings = hooks("/opt/hagency", "/work/space").unwrap();
        let hook = &settings["PreToolUse"][0];
        assert_eq!(hook["matcher"], MATCHER);
        assert_eq!(
            hook["hooks"][0]["command"],
            "'/opt/hagency' claude-write-guard '/work/space'"
        );
        assert!(hooks("relative", "/w").is_none());
        assert!(hooks("/opt/it's", "/w").is_none());
        assert!(hooks("/opt/hagency", "/w\nx").is_none());
    }
}
