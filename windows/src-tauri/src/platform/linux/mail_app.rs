// The user's mail app, for "Send by email" without Resend: the email is
// written there, file attached, and the user sends it.
//
// Two things stand in the way of the obvious `xdg-email --attach`:
//   * a mail app from a snap or a flatpak cannot read Coucou's inbox
//     (~/.local/share/coucou/inbox): snaps never see hidden folders of the
//     home, flatpaks only their own. The file is copied first into the app's
//     own data folder (~/snap/<name>/common, ~/.var/app/<id>/cache), which it
//     always can read, and nothing is left in the user's folders;
//   * xdg-email hands Thunderbird the path through an unquoted `echo`, which
//     mangles names with spaces. Thunderbird is called directly instead, with
//     the attachment as a proper file:// URL.
// Anything else goes through xdg-email as before.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::super::home_dir;
use super::find_on_path;

/// Copies older than this are swept on the next email: the mail app reads
/// the file when it sends, so it may not go right away.
const KEEP_COPIES: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq)]
pub enum Sandbox {
    None,
    Snap(String),
    Flatpak(String),
}

#[derive(Debug, PartialEq)]
pub struct MailApp {
    /// The command line from the desktop file, field codes removed.
    pub exec: Vec<String>,
    pub sandbox: Sandbox,
    pub thunderbird: bool,
}

/// The desktop file id of the mailto: handler, as xdg-mime says.
fn default_handler() -> Option<String> {
    let exe = find_on_path("xdg-mime")?;
    let out = Command::new(exe).args(["query", "default", "x-scheme-handler/mailto"]).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (id.ends_with(".desktop") && !id.contains('/')).then_some(id)
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| d.starts_with('/')).map(PathBuf::from));
    // Snap and flatpak desktop files, should the session not list them.
    for d in ["/var/lib/snapd/desktop", "/var/lib/flatpak/exports/share"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(home_dir().join(".local/share/flatpak/exports/share"));
    dirs
}

/// The `Exec=` line of the [Desktop Entry] group.
pub fn exec_line(desktop: &str) -> Option<String> {
    let mut in_entry = false;
    for line in desktop.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(exec) = line.strip_prefix("Exec=") {
                return Some(exec.to_string());
            }
        }
    }
    None
}

/// Splits an Exec value into words (double quotes only, as the spec has it)
/// and drops field codes such as %u.
pub fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.retain(|w| !(w.len() == 2 && w.starts_with('%')));
    words
}

/// Where the app comes from, from its command line.
pub fn sandbox_of(exec: &[String]) -> Sandbox {
    let Some(program) = exec.first() else { return Sandbox::None };
    if let Some(name) = program.strip_prefix("/snap/bin/") {
        return Sandbox::Snap(name.split('.').next().unwrap_or(name).to_string());
    }
    if Path::new(program).file_name().is_some_and(|n| n == "flatpak") && exec.get(1).is_some_and(|w| w == "run") {
        // The app id is the first word after `run` that is not an option.
        if let Some(id) = exec.iter().skip(2).find(|w| !w.starts_with('-')) {
            return Sandbox::Flatpak(id.clone());
        }
    }
    // Ubuntu's transitional /usr/bin/thunderbird and friends start the snap.
    let base = Path::new(program).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if !base.is_empty() && Path::new("/snap/bin").join(&base).exists() && !program.starts_with('/') {
        return Sandbox::Snap(base);
    }
    Sandbox::None
}

pub fn find() -> Option<MailApp> {
    let id = default_handler()?;
    let desktop = data_dirs()
        .into_iter()
        .map(|d| d.join("applications").join(&id))
        .find_map(|p| std::fs::read_to_string(p).ok())?;
    let exec = exec_words(&exec_line(&desktop)?);
    if exec.is_empty() {
        return None;
    }
    let sandbox = sandbox_of(&exec);
    let thunderbird = id.to_lowercase().contains("thunderbird")
        || exec.iter().any(|w| Path::new(w).file_name().is_some_and(|n| n.to_string_lossy().contains("thunderbird")));
    Some(MailApp { exec, sandbox, thunderbird })
}

/// A folder the app can read, or None when it can read the inbox itself:
/// the sandbox's own root in the home, and the copies' folder inside it.
fn readable_dir(sandbox: &Sandbox) -> Option<(PathBuf, PathBuf)> {
    let root = match sandbox {
        Sandbox::None => return None,
        Sandbox::Snap(name) => home_dir().join("snap").join(name),
        Sandbox::Flatpak(id) => home_dir().join(".var/app").join(id),
    };
    let dir = match sandbox {
        Sandbox::Snap(_) => root.join("common").join("coucou-mail"),
        _ => root.join("cache").join("coucou-mail"),
    };
    Some((root, dir))
}

/// The sandboxed app can write in its own folder, so it could plant a
/// symlink there to make Coucou — which runs outside the sandbox — write or
/// delete somewhere else. Every step from the sandbox's root down must be a
/// real folder, and the folder must really be where it says.
fn checked_dir(root: &Path, dir: &Path) -> Result<PathBuf, String> {
    let refuse = || "The mail app's folder looks tampered with; the file was not attached.".to_string();
    let mut at = root.to_path_buf();
    let steps: Vec<_> = dir.strip_prefix(root).map_err(|_| refuse())?.components().collect();
    for step in std::iter::once(None).chain(steps.into_iter().map(Some)) {
        if let Some(step) = step {
            at.push(step);
        }
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => return Err(refuse()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&at).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
            }
            Err(e) => return Err(format!("Could not prepare the attachment: {e}")),
        }
    }
    let real = dir.canonicalize().map_err(|_| refuse())?;
    let expected = root.canonicalize().map_err(|_| refuse())?.join(dir.strip_prefix(root).map_err(|_| refuse())?);
    if real != expected {
        return Err(refuse());
    }
    Ok(real)
}

/// The attachment where the mail app can read it: the inbox file itself, or
/// a copy in the app's own folder, in a fresh folder of its own so the name
/// stays. Nothing on the way follows a symlink.
pub fn stage(file: &Path, sandbox: &Sandbox) -> Result<PathBuf, String> {
    let Some((root, dir)) = readable_dir(sandbox) else { return Ok(file.to_path_buf()) };
    stage_in(file, &root, &dir)
}

fn stage_in(file: &Path, root: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dir = checked_dir(root, dir)?;
    sweep(&dir);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let slot = dir.join(stamp.to_string());
    // create_dir, not create_dir_all: it fails on anything already there.
    std::fs::create_dir(&slot).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    let name = file.file_name().ok_or("The file has no name.")?;
    let copy = slot.join(name);
    // create_new is O_EXCL: it never opens through a symlink planted meanwhile.
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&copy)
        .map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    let mut src = std::fs::File::open(file).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    std::io::copy(&mut src, &mut out).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    Ok(copy)
}

/// Removes copies older than a day: only our own numbered folders, never a
/// symlink (remove_dir_all does not follow the ones inside).
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let ours = entry.file_name().to_str().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !ours || meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let old = meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEEP_COPIES);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A file:// URL with everything but unreserved characters and slashes
/// percent-encoded.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => url.push(*b as char),
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// Thunderbird's `-compose` value. Its values sit in single quotes with no way
/// to escape one, so a quote in the text becomes a typographic one.
pub fn thunderbird_compose(to: &str, subject: &str, body: &str, attachment: Option<&Path>) -> String {
    let clean = |s: &str| s.replace('\'', "’");
    let mut fields = vec![format!("to='{}'", clean(to)), format!("subject='{}'", clean(subject))];
    if !body.is_empty() {
        fields.push(format!("body='{}'", clean(body)));
    }
    if let Some(file) = attachment {
        fields.push(format!("attachment='{}'", file_url(file)));
    }
    fields.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine's mail app: `cargo test --lib -- --ignored --nocapture real_mail_app`.
    #[test]
    #[ignore]
    fn real_mail_app() {
        println!("{:?}", find());
    }

    #[test]
    fn desktop_files_give_their_command_line() {
        let desktop = "[Desktop Entry]\nName=Thunderbird\nExec=/snap/bin/thunderbird %u\n\n[Desktop Action ComposeMessage]\nExec=/snap/bin/thunderbird -compose\n";
        let exec = exec_words(&exec_line(desktop).unwrap());
        assert_eq!(exec, ["/snap/bin/thunderbird"]);
        assert_eq!(exec_words(r#""/opt/My Mail/mail" --new %U"#), ["/opt/My Mail/mail", "--new"]);
    }

    #[test]
    fn sandboxes_are_told_apart() {
        let words = |s: &str| exec_words(s);
        assert_eq!(sandbox_of(&words("/snap/bin/thunderbird %u")), Sandbox::Snap("thunderbird".into()));
        assert_eq!(
            sandbox_of(&words("/usr/bin/flatpak run --branch=stable --command=thunderbird org.mozilla.Thunderbird %u")),
            Sandbox::Flatpak("org.mozilla.Thunderbird".into())
        );
        assert_eq!(sandbox_of(&words("/usr/lib/evolution/evolution %u")), Sandbox::None);
    }

    #[test]
    fn attachments_become_proper_file_urls() {
        assert_eq!(
            file_url(Path::new("/home/u/snap/thunderbird/common/coucou-mail/1/Лаба 3 (1).pdf")),
            "file:///home/u/snap/thunderbird/common/coucou-mail/1/%D0%9B%D0%B0%D0%B1%D0%B0%203%20%281%29.pdf"
        );
    }

    #[test]
    fn the_compose_value_keeps_every_field_in_its_quotes() {
        let v = thunderbird_compose("a@b.co", "Lab's report", "Hi, it's here", Some(Path::new("/x/a b.pdf")));
        assert_eq!(v, "to='a@b.co',subject='Lab’s report',body='Hi, it’s here',attachment='file:///x/a%20b.pdf'");
        assert_eq!(thunderbird_compose("a@b.co", "S", "", None), "to='a@b.co',subject='S'");
    }

    #[test]
    fn a_sandboxed_app_gets_a_copy_it_can_read() {
        let tmp = std::env::temp_dir().join(format!("coucou-mail-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("note.txt");
        std::fs::write(&file, "hi").unwrap();
        assert_eq!(stage(&file, &Sandbox::None).unwrap(), file);

        // A sandbox root with the copies' folder made on the way.
        let root = tmp.join("snap/thunderbird");
        std::fs::create_dir_all(&root).unwrap();
        let dir = root.join("common/coucou-mail");
        let copy = stage_in(&file, &root, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "hi");
        assert!(copy.starts_with(root.canonicalize().unwrap()));

        // A symlink planted by the sandboxed app is refused, and nothing lands
        // where it points.
        let elsewhere = tmp.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let evil_root = tmp.join("snap/evil");
        std::fs::create_dir_all(evil_root.join("common")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, evil_root.join("common/coucou-mail")).unwrap();
        assert!(stage_in(&file, &evil_root, &evil_root.join("common/coucou-mail")).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        // The sweep leaves symlinks and foreign names alone.
        std::os::unix::fs::symlink(&elsewhere, dir.join("1")).unwrap();
        std::fs::create_dir(dir.join("notes")).unwrap();
        sweep(&dir);
        assert!(dir.join("notes").exists());
        assert!(elsewhere.exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
