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

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
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

/// The path, below the home, of the folder a sandboxed app can read, or None
/// when it can read the inbox itself.
fn readable_dir(sandbox: &Sandbox) -> Option<Vec<String>> {
    let parts = match sandbox {
        Sandbox::None => return None,
        Sandbox::Snap(name) => ["snap", name.as_str(), "common", "coucou-mail"].map(String::from).to_vec(),
        Sandbox::Flatpak(id) => [".var", "app", id.as_str(), "cache", "coucou-mail"].map(String::from).to_vec(),
    };
    // A name from a desktop file must stay one plain path component.
    parts.iter().all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains('/') && !p.contains('\0')).then_some(parts)
}

// The sandboxed app can write in its own folder, so it can plant or swap a
// symlink there at any moment to make Coucou — which runs outside the
// sandbox — write or delete somewhere else. Checking paths first and using
// them after leaves a window for that swap. So nothing here goes through a
// path: every step is openat() on the folder opened before it, with
// O_NOFOLLOW, and the files are made and removed relative to those handles.

fn c_name(name: &str) -> Result<CString, String> {
    CString::new(name).map_err(|_| "Bad file name.".to_string())
}

fn last_error(what: &str) -> String {
    format!("Could not prepare the attachment ({what}): {}", std::io::Error::last_os_error())
}

/// Opens folder `name` inside `parent`, never through a symlink; makes it
/// first when `create` and it is missing.
fn open_dir_at(parent: RawFd, name: &str, create: bool) -> Result<OwnedFd, String> {
    let c = c_name(name)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut fd = unsafe { libc::openat(parent, c.as_ptr(), flags) };
    if fd < 0 && create && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        if unsafe { libc::mkdirat(parent, c.as_ptr(), 0o700) } < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(last_error(name));
        }
        fd = unsafe { libc::openat(parent, c.as_ptr(), flags) };
    }
    if fd < 0 {
        // ELOOP: a symlink stood where a folder should be.
        return Err("The mail app's folder looks tampered with; the file was not attached.".into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The attachment where the mail app can read it: the inbox file itself, or
/// a copy in the app's own folder, in a fresh folder of its own so the name
/// stays.
pub fn stage(file: &Path, sandbox: &Sandbox) -> Result<PathBuf, String> {
    let Some(parts) = readable_dir(sandbox) else { return Ok(file.to_path_buf()) };
    stage_in(file, &home_dir(), &parts)
}

fn stage_in(file: &Path, home: &Path, parts: &[String]) -> Result<PathBuf, String> {
    let dir = open_copies_dir(home, parts)?;
    sweep(dir.as_raw_fd());
    let name = file.file_name().and_then(|n| n.to_str()).ok_or("The file has no usable name.")?.to_string();
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).to_string();
    let slot_c = c_name(&stamp)?;
    // mkdirat fails on anything already there, symlink included.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), slot_c.as_ptr(), 0o700) } < 0 {
        return Err(last_error("folder"));
    }
    let slot = open_dir_at(dir.as_raw_fd(), &stamp, false)?;
    let name_c = c_name(&name)?;
    let fd = unsafe {
        libc::openat(
            slot.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(last_error("file"));
    }
    let mut out = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let mut src = std::fs::File::open(file).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    std::io::copy(&mut src, &mut out).map_err(|e| format!("Could not prepare the attachment: {e}"))?;
    // The path is only for the mail app, which reads it inside its own sandbox.
    let mut path = home.to_path_buf();
    path.extend(parts);
    Ok(path.join(stamp).join(name))
}

/// The copies' folder, walked down from `root` (the home) one component at
/// a time. The sandbox's own folder must exist already; only the last two
/// steps, ours, are made.
fn open_copies_dir(root: &Path, parts: &[String]) -> Result<OwnedFd, String> {
    let c = CString::new(root.as_os_str().as_encoded_bytes()).map_err(|_| "Bad home folder.".to_string())?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(last_error("home"));
    }
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    for (i, part) in parts.iter().enumerate() {
        dir = open_dir_at(dir.as_raw_fd(), part, i + 2 >= parts.len())?;
    }
    Ok(dir)
}

/// Entries of an opened folder, read through its handle.
fn entries(dir: RawFd) -> Vec<String> {
    std::fs::read_dir(format!("/proc/self/fd/{dir}"))
        .map(|it| it.flatten().filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default()
}

/// Removes copies older than a day: only our own numbered folders, each
/// opened without following symlinks and emptied through its handle.
fn sweep(dir: RawFd) {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    for name in entries(dir) {
        let Ok(stamp) = name.parse::<u128>() else { continue };
        if now.saturating_sub(stamp) < KEEP_COPIES.as_millis() {
            continue;
        }
        let Ok(slot) = open_dir_at(dir, &name, false) else { continue };
        for inner in entries(slot.as_raw_fd()) {
            if let Ok(c) = c_name(&inner) {
                // Files and symlinks alike are unlinked, never followed.
                unsafe { libc::unlinkat(slot.as_raw_fd(), c.as_ptr(), 0) };
            }
        }
        if let Ok(c) = c_name(&name) {
            unsafe { libc::unlinkat(dir, c.as_ptr(), libc::AT_REMOVEDIR) };
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

        // The sandbox's folder exists; the copies' folders are made on the way.
        let parts: Vec<String> = ["snap", "thunderbird", "common", "coucou-mail"].map(String::from).to_vec();
        std::fs::create_dir_all(tmp.join("snap/thunderbird")).unwrap();
        let copy = stage_in(&file, &tmp, &parts).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "hi");
        assert!(copy.starts_with(tmp.join("snap/thunderbird/common/coucou-mail")));

        // A symlink planted by the sandboxed app is refused, and nothing lands
        // where it points.
        let elsewhere = tmp.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(tmp.join("snap/evil/common")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, tmp.join("snap/evil/common/coucou-mail")).unwrap();
        let evil: Vec<String> = ["snap", "evil", "common", "coucou-mail"].map(String::from).to_vec();
        assert!(stage_in(&file, &tmp, &evil).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        // The sweep leaves symlinks, fresh copies and foreign names alone, and
        // never deletes through a link.
        let dir = tmp.join("snap/thunderbird/common/coucou-mail");
        std::fs::write(elsewhere.join("keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("1")).unwrap();
        std::fs::create_dir(dir.join("notes")).unwrap();
        std::fs::create_dir(dir.join("2")).unwrap();
        std::fs::write(dir.join("2/old.txt"), "x").unwrap();
        let handle = open_copies_dir(&tmp, &parts).unwrap();
        sweep(handle.as_raw_fd());
        assert!(dir.join("notes").exists());
        assert!(elsewhere.join("keep.txt").exists());
        assert!(!dir.join("2").exists(), "an old copy goes");
        assert!(copy.exists(), "a fresh copy stays");
        assert!(readable_dir(&Sandbox::Snap("../x".into())).is_none());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
