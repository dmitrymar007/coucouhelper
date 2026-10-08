// Coucou's hooks for Cline (the VS Code extension): its tasks on the island.
//
// Cline runs, for each event, an executable named after it in its global hooks
// folder, `<Documents>/Cline/Hooks/` (the folder `xdg-user-dir DOCUMENTS`
// names, as Cline asks it). Installing writes one small script per event the
// island shows, each handing Cline's JSON to the relay (`coucou-hook --agent
// cline`), which turns it into Claude Code's shape. A script of the same name
// that is not Coucou's is never replaced, and removing deletes only ours.
// Only ever from a click in Settings.
//
// Cline's hooks can stop a task but not approve a tool: approvals stay in
// Cline's own panel.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{platform, settings};

/// Cline's events the island shows. Notification and TaskError are in Cline's
/// list but not raised by Cline 4; PreCompact is not a step worth showing.
pub const EVENTS: &[&str] = &[
    "TaskStart",
    "TaskResume",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "TaskComplete",
    "TaskCancel",
];

/// The line that makes a script ours.
const MARKER: &str = "# Written by Coucou";

/// The user's documents folder, as Cline finds it: `xdg-user-dir DOCUMENTS`
/// (`~/Документы` on a Russian desktop), else `~/Documents`.
fn documents_dir() -> PathBuf {
    std::process::Command::new("xdg-user-dir")
        .arg("DOCUMENTS")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| platform::home_dir().join("Documents"))
}

pub fn hooks_dir() -> PathBuf {
    documents_dir().join("Cline").join("Hooks")
}

/// Cline is installed in VS Code (or a fork of it), or has run once.
fn cline_present() -> bool {
    let home = platform::home_dir();
    let in_editor = [".vscode", ".vscode-oss", ".vscode-insiders", ".cursor", ".windsurf"].iter().any(|editor| {
        std::fs::read_dir(home.join(editor).join("extensions"))
            .map(|entries| {
                entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("saoudrizwan.claude-dev-"))
            })
            .unwrap_or(false)
    });
    in_editor || home.join(".cline").is_dir()
}

/// `s` as one single-quoted shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The script for one event. Cline waits for it, so it never waits itself:
/// the relay gives up at once when Coucou is not running, and the answer is
/// always "carry on".
pub fn script(relay: &str, event: &str) -> String {
    format!(
        "#!/bin/sh\n\
         {MARKER} (Settings → Cline): shows this Cline task on the Coucou island.\n\
         # Removing Cline in Coucou's Settings deletes it.\n\
         {} --agent cline {event} >/dev/null 2>&1\n\
         echo '{{\"cancel\":false}}'\n",
        sh_quote(relay)
    )
}

fn relay() -> String {
    settings::hook_exe_path().to_string_lossy().to_string()
}

fn is_ours(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(|l| l.starts_with(MARKER)))
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Cline is installed.
    pub cline: bool,
    /// Every one of our scripts is there.
    pub installed: bool,
    /// Some of ours are there.
    pub partial: bool,
    pub up_to_date: bool,
    /// Events whose script is somebody else's: Coucou leaves them alone.
    pub taken: Vec<String>,
    pub path: String,
}

pub fn status() -> Status {
    status_in(&hooks_dir(), &relay())
}

fn status_in(dir: &Path, relay: &str) -> Status {
    let mut ours = 0;
    let mut current = 0;
    let mut taken = Vec::new();
    for event in EVENTS {
        let path = dir.join(event);
        if !path.exists() {
            continue;
        }
        if !is_ours(&path) {
            taken.push(event.to_string());
            continue;
        }
        ours += 1;
        if std::fs::read_to_string(&path).is_ok_and(|t| t == script(relay, event)) && is_executable(&path) {
            current += 1;
        }
    }
    let wanted = EVENTS.len() - taken.len();
    Status {
        cline: cline_present(),
        installed: ours > 0 && ours == wanted,
        partial: ours > 0 && ours < wanted,
        up_to_date: ours == wanted && current == wanted,
        taken,
        path: dir.to_string_lossy().to_string(),
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o100 != 0)
}

pub fn install() -> Result<Status, String> {
    install_in(&hooks_dir(), &relay())?;
    crate::log::line("Cline hooks installed");
    Ok(status())
}

fn install_in(dir: &Path, relay: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for event in EVENTS {
        let path = dir.join(event);
        if path.exists() && !is_ours(&path) {
            continue;
        }
        // Written beside it and renamed over it: Cline never runs half a script.
        let temp = dir.join(format!(".{event}.coucou-{}", std::process::id()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&temp)
            .and_then(|mut f| f.write_all(script(relay, event).as_bytes()))
            .and_then(|_| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
            })
            .and_then(|_| std::fs::rename(&temp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("{}: {e}", path.display()));
        }
    }
    Ok(())
}

pub fn remove() -> Result<Status, String> {
    remove_in(&hooks_dir())?;
    crate::log::line("Cline hooks removed");
    Ok(status())
}

fn remove_in(dir: &Path) -> Result<(), String> {
    for event in EVENTS {
        let path = dir.join(event);
        if !is_ours(&path) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_runs_the_relay_as_one_word_and_always_lets_cline_go_on() {
        let s = script("/home/it's me/.local/share/coucou/bin/coucou-hook", "PreToolUse");
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains(r"'/home/it'\''s me/.local/share/coucou/bin/coucou-hook' --agent cline PreToolUse >/dev/null 2>&1"));
        assert!(s.ends_with("echo '{\"cancel\":false}'\n"));
        assert!(s.lines().any(|l| l.starts_with(MARKER)));
    }

    #[test]
    fn installing_leaves_someone_elses_hook_alone_and_removing_takes_only_ours() {
        const RELAY: &str = "/opt/relay/coucou-hook";
        let dir = std::env::temp_dir().join(format!("coucou-cline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let theirs = "#!/bin/sh\necho mine\n";
        std::fs::write(dir.join("PreToolUse"), theirs).unwrap();

        install_in(&dir, RELAY).unwrap();
        let st = status_in(&dir, RELAY);
        assert_eq!(st.taken, vec!["PreToolUse".to_string()]);
        assert!(st.installed && st.up_to_date, "every other event is ours and current");
        assert_eq!(std::fs::read_to_string(dir.join("PreToolUse")).unwrap(), theirs);
        assert!(is_executable(&dir.join("TaskStart")));

        // An old script of ours is replaced on the next install.
        std::fs::write(dir.join("TaskComplete"), format!("#!/bin/sh\n{MARKER} long ago\n")).unwrap();
        assert!(!status_in(&dir, RELAY).up_to_date);
        install_in(&dir, RELAY).unwrap();
        assert!(status_in(&dir, RELAY).up_to_date);

        remove_in(&dir).unwrap();
        let st = status_in(&dir, RELAY);
        assert!(!st.installed && !st.partial);
        assert_eq!(std::fs::read_to_string(dir.join("PreToolUse")).unwrap(), theirs);
        assert!(!dir.join("TaskStart").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
