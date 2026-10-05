// The Coucou plugin for opencode (opencode-plugin/coucou.js): it shows
// opencode's sessions and permission requests on the island, through the same
// relay as Claude Code's hooks.
//
// Installing it writes one file of ours, ~/.config/opencode/plugin/coucou.js,
// with the relay's path filled in; removing it deletes that file. Nothing of
// the user's opencode config is read or changed. Only ever from a click in
// Settings.

use std::path::PathBuf;

use serde::Serialize;

use crate::{opencode_cli, platform, settings};

const SOURCE: &str = include_str!("../../opencode-plugin/coucou.js");
const PLACEHOLDER: &str = "\"__COUCOU_RELAY__\"";

/// opencode's config folder: $XDG_CONFIG_HOME/opencode, else ~/.config/opencode
/// (on Windows too — opencode uses the same layout there).
fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| platform::home_dir().join(".config"))
        .join("opencode")
}

pub fn plugin_path() -> PathBuf {
    config_dir().join("plugin").join("coucou.js")
}

/// The plugin as written to disk: the relay's path as a JavaScript string.
pub fn contents(relay: &str) -> String {
    let literal = serde_json::to_string(relay).expect("a string always serializes");
    SOURCE.replacen(PLACEHOLDER, &literal, 1)
}

fn expected() -> String {
    contents(&settings::hook_exe_path().to_string_lossy())
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// opencode is installed.
    pub opencode: bool,
    pub installed: bool,
    pub up_to_date: bool,
    pub path: String,
}

pub fn status() -> Status {
    let path = plugin_path();
    let on_disk = std::fs::read_to_string(&path).ok();
    Status {
        opencode: opencode_cli::find_opencode().is_some(),
        installed: on_disk.is_some(),
        up_to_date: on_disk.is_some_and(|text| text == expected()),
        path: path.to_string_lossy().to_string(),
    }
}

pub fn install() -> Result<Status, String> {
    let path = plugin_path();
    let dir = path.parent().ok_or("no plugin folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, expected()).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::log::line("opencode plugin installed");
    Ok(status())
}

pub fn remove() -> Result<Status, String> {
    match std::fs::remove_file(plugin_path()) {
        Ok(()) => crate::log::line("opencode plugin removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_path_is_written_as_a_javascript_string() {
        let js = contents("/home/a \"b\"/.local/share/coucou/bin/coucou-hook");
        assert!(js.contains(r#"const RELAY = "/home/a \"b\"/.local/share/coucou/bin/coucou-hook";"#));
        assert!(!js.contains("__COUCOU_RELAY__"));
    }

    #[test]
    fn the_plugin_stays_out_of_coucous_own_chat() {
        assert!(SOURCE.contains("if (process.env.COUCOU_CHAT) return {};"));
        assert!(SOURCE.contains(r#"const AGENT = "opencode";"#));
    }
}
