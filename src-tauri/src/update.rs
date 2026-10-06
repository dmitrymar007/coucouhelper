// Updates from this project's GitHub releases, for a packaged install only.
//
// Every few hours Coucou asks GitHub for the latest release (one public API
// call, nothing sent but the request itself; Settings → Updates turns it off).
// A newer version shows up in the tray, in Settings and once on the island.
// Installing it is always a click: the .deb is downloaded, checked against the
// release's SHA256SUMS, installed through pkexec (GNOME asks for the password
// in its own dialog) and Coucou starts again on the new version.
//
// A build run from a source tree is not touched: the package would replace
// /usr/bin/coucou, not the binary actually running.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::log;

const REPO: &str = "dmitrymar007/coucouhelper";
const PACKAGED_EXE: &str = "/usr/bin/coucou";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// A packaged install: updates apply to it.
    pub applicable: bool,
    pub current: String,
    /// The latest release's version, once checked.
    pub latest: Option<String>,
    pub available: bool,
    /// Seconds since 1970 of the last check that got an answer.
    pub checked_at: Option<u64>,
    pub checking: bool,
    pub installing: bool,
    pub error: Option<String>,
    /// The latest release's notes (its changelog section).
    pub notes: String,
}

struct Release {
    version: String,
    deb_name: String,
    deb_url: String,
    sums_url: String,
}

#[derive(Default)]
pub struct Updates {
    status: Mutex<UpdateStatus>,
    release: Mutex<Option<Release>>,
    /// The version already announced on the island this run.
    announced: Mutex<Option<String>>,
}

/// True when the running binary is the one the .deb installed.
pub fn packaged() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .is_some_and(|p| p == Path::new(PACKAGED_EXE))
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let mut s = app.state::<Updates>().status.lock().unwrap().clone();
    s.applicable = packaged();
    s.current = env!("CARGO_PKG_VERSION").to_string();
    s
}

fn publish(app: &AppHandle) {
    let s = status(app);
    crate::tray::set_update(app, s.available.then(|| s.latest.clone()).flatten().as_deref());
    let _ = app.emit("update-status", s);
}

fn edit(app: &AppHandle, f: impl FnOnce(&mut UpdateStatus)) {
    f(&mut app.state::<Updates>().status.lock().unwrap());
    publish(app);
}

/// Checks now and every few hours, while the setting allows it.
pub fn start(app: AppHandle) {
    publish(&app);
    if !packaged() {
        return;
    }
    crate::platform::drop_stale_shell_extension();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let wanted = app.state::<crate::Shared>().settings.lock().unwrap().auto_update;
            if wanted && !crate::integrations::PAUSED.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = check(&app).await;
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// "0.2.10" → (0, 2, 10); anything else is not a version.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

fn deb_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Asks GitHub for the latest release and says whether it is newer.
pub async fn check(app: &AppHandle) -> Result<(), String> {
    edit(app, |s| {
        s.checking = true;
        s.error = None;
    });
    let result = latest_release().await;
    let current = env!("CARGO_PKG_VERSION");
    match result {
        Ok((release, notes)) => {
            let available = newer(&release.version, current);
            let version = release.version.clone();
            *app.state::<Updates>().release.lock().unwrap() = available.then_some(release);
            edit(app, |s| {
                s.checking = false;
                s.latest = Some(version.clone());
                s.available = available;
                s.notes = notes;
                s.checked_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
            });
            if available {
                let first = {
                    let updates = app.state::<Updates>();
                    let mut announced = updates.announced.lock().unwrap();
                    let first = announced.as_deref() != Some(version.as_str());
                    *announced = Some(version.clone());
                    first
                };
                if first {
                    log::line(format!("update: {version} is out"));
                    let _ = app.emit_to(crate::island::WINDOW_LABEL, "update-available", version);
                }
            }
            Ok(())
        }
        Err(err) => {
            log::line(format!("update check failed: {err}"));
            edit(app, |s| {
                s.checking = false;
                s.error = Some(err.clone());
            });
            Err(err)
        }
    }
}

async fn latest_release() -> Result<(Release, String), String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = http()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GitHub unreachable: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    let tag = body.get("tag_name").and_then(Value::as_str).unwrap_or("");
    let version = tag.strip_prefix("linux-v").unwrap_or(tag).to_string();
    if parse_version(&version).is_none() {
        return Err(format!("the latest release, {tag}, is not a Coucou for Linux version"));
    }
    let deb_name = format!("Coucou-Linux-{version}-{}.deb", deb_arch());
    let assets = body.get("assets").and_then(Value::as_array).cloned().unwrap_or_default();
    let url_of = |name: &str| {
        assets.iter().find_map(|a| {
            (a.get("name").and_then(Value::as_str) == Some(name))
                .then(|| a.get("browser_download_url").and_then(Value::as_str).map(str::to_string))
                .flatten()
        })
    };
    let deb_url = url_of(&deb_name).ok_or_else(|| format!("the release has no {deb_name}"))?;
    let sums_url = url_of("SHA256SUMS").ok_or("the release has no SHA256SUMS")?;
    let notes = body.get("body").and_then(Value::as_str).unwrap_or("").to_string();
    Ok((Release { version, deb_name, deb_url, sums_url }, notes))
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::platform::home_dir().join(".cache"))
        .join("coucou")
        .join("update")
}

/// Downloads, checks and installs the newer release, then starts it.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    if !packaged() {
        return Err("Only a Coucou installed from its package updates itself.".into());
    }
    let release = {
        let updates = app.state::<Updates>();
        let held = updates.release.lock().unwrap();
        held.as_ref().map(|r| (r.version.clone(), r.deb_name.clone(), r.deb_url.clone(), r.sums_url.clone()))
    };
    let Some((version, deb_name, deb_url, sums_url)) = release else {
        return Err("No update to install.".into());
    };
    edit(app, |s| {
        s.installing = true;
        s.error = None;
    });
    let result = download_and_install(&version, &deb_name, &deb_url, &sums_url).await;
    match result {
        Ok(()) => {
            log::line(format!("update: {version} installed, restarting"));
            restart(app);
            Ok(())
        }
        Err(err) => {
            log::line(format!("update to {version} failed: {err}"));
            edit(app, |s| {
                s.installing = false;
                s.error = Some(err.clone());
            });
            Err(err)
        }
    }
}

async fn download_and_install(version: &str, deb_name: &str, deb_url: &str, sums_url: &str) -> Result<(), String> {
    let dir = cache_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let client = http();
    let get = |url: String| {
        let client = client.clone();
        async move {
            let response = client.get(&url).send().await.map_err(|e| format!("download failed: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("download failed: {}", response.status()));
            }
            response.bytes().await.map_err(|e| format!("download failed: {e}"))
        }
    };
    let sums = get(sums_url.to_string()).await?;
    let expected = String::from_utf8_lossy(&sums)
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == deb_name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("SHA256SUMS does not list {deb_name}"))?;
    let deb = get(deb_url.to_string()).await?;
    let path = dir.join(deb_name);
    std::fs::write(&path, &deb).map_err(|e| format!("{}: {e}", path.display()))?;

    // coreutils' sha256sum, rather than one more crate for one hash. This
    // check only spares a password prompt for a broken download: the one
    // that counts runs as root, below.
    let out = tokio::process::Command::new("sha256sum")
        .arg(&path)
        .output()
        .await
        .map_err(|e| format!("sha256sum: {e}"))?;
    let actual = String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if actual != expected {
        return Err(format!("the downloaded {deb_name} does not match its checksum"));
    }
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("SHA256SUMS holds no proper checksum".into());
    }

    log::line(format!("update: installing {version} through pkexec"));
    let status = tokio::process::Command::new("pkexec")
        .args(["/bin/sh", "-c", ROOT_INSTALL, "coucou-update"])
        .arg(&path)
        .arg(&expected)
        .status()
        .await
        .map_err(|e| format!("pkexec: {e}"))?;
    match status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 = the password dialog was dismissed, 127 = not authorised.
        Some(126) | Some(127) => Err("Installation cancelled.".into()),
        Some(3) => Err(format!("the {deb_name} handed to apt did not match its checksum; nothing was installed")),
        code => Err(format!("apt-get failed ({code:?}).")),
    }
}

/// Run by root through pkexec, with the downloaded package ($1) and its
/// checksum ($2). The download sits in the user's cache, where anything
/// running as the user could swap it between our check and apt reading it —
/// and root would then install whatever was put there. So root copies it
/// into a directory only root can write, checks that copy, and installs that
/// very file. $1 and $2 are arguments, never part of the script text.
const ROOT_INSTALL: &str = r#"set -eu
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
cp -- "$1" "$d/update.deb"
printf '%s  %s\n' "$2" "$d/update.deb" | sha256sum -c --status || exit 3
/usr/bin/apt-get install -y "$d/update.deb"
"#;

/// Starts the new /usr/bin/coucou once this one is gone, then quits.
fn restart(app: &AppHandle) {
    // setsid: the new Coucou must not die with this one. The pause lets this
    // one release the single-instance lock and the relay socket first.
    let spawned = std::process::Command::new("setsid")
        .args(["-f", "/bin/sh", "-c", &format!("sleep 2; exec {PACKAGED_EXE}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(err) = spawned {
        log::line(format!("update: could not restart ({err}); start Coucou again by hand"));
    }
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.2.10", "0.2.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.2.3", "0.2.3"));
        assert!(!newer("0.2.2", "0.2.3"));
        assert!(!newer("latest", "0.2.3"));
        assert_eq!(parse_version("0.2.3.1"), None);
    }
}
