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

/// A folder the app can read, or None when it can read the inbox itself.
fn readable_dir(sandbox: &Sandbox) -> Option<PathBuf> {
    match sandbox {
        Sandbox::None => None,
        Sandbox::Snap(name) => Some(home_dir().join("snap").join(name).join("common").join("coucou-mail")),
        Sandbox::Flatpak(id) => Some(home_dir().join(".var/app").join(id).join("cache").join("coucou-mail")),
    }
}

/// The attachment where the mail app can read it: the inbox file itself, or
/// a copy in the app's own folder, in a folder of its own so the name stays.
pub fn stage(file: &Path, sandbox: &Sandbox) -> Result<PathBuf, String> {
    let Some(dir) = readable_dir(sandbox) else { return Ok(file.to_path_buf()) };
    sweep(&dir);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let slot = dir.join(stamp.to_string());
    std::fs::create_dir_all(&slot).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    let name = file.file_name().ok_or("The file has no name.")?;
    let copy = slot.join(name);
    std::fs::copy(file, &copy).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    Ok(copy)
}

/// Removes copies older than a day.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP_COPIES);
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
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
