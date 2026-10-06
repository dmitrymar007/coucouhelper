// Preferences, stored as plain JSON in settings.json under platform::config_dir().
// No secret ever lands here — API keys live in the OS keychain (see secrets.rs).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    /// Claude model used by the chat. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
    /// Who answers the chat: "claude-code" (the user's subscription, through
    /// the `claude` CLI), "opencode" (the `opencode` CLI and its providers) or
    /// "api-key".
    #[serde(default = "default_chat_backend")]
    pub chat_backend: String,
    /// Model alias handed to Claude Code: "sonnet", "opus", "haiku"…
    #[serde(default = "default_cli_model")]
    pub cli_model: String,
    /// "provider/model" handed to opencode; empty: opencode's own default.
    #[serde(default)]
    pub opencode_model: String,
    /// Minutes without a message before the Claude Code process stops; 0 never.
    #[serde(default = "default_chat_idle_minutes")]
    pub chat_idle_minutes: u32,
    /// Last Claude Code or opencode chat session, so it can be picked up after
    /// a restart.
    #[serde(default)]
    pub last_chat_session: Option<String>,
    /// The island has told the user once about the GNOME extension.
    #[serde(default)]
    pub shell_hint_shown: bool,
    /// Look for a newer release on GitHub every few hours (packaged installs).
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

fn default_true() -> bool {
    true
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

fn default_chat_backend() -> String {
    crate::chat::BACKEND_CLAUDE_CODE.to_string()
}

fn default_cli_model() -> String {
    crate::claude_cli::DEFAULT_MODEL.to_string()
}

fn default_chat_idle_minutes() -> u32 {
    30
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            model: default_model(),
            chat_backend: default_chat_backend(),
            cli_model: default_cli_model(),
            opencode_model: String::new(),
            chat_idle_minutes: default_chat_idle_minutes(),
            last_chat_session: None,
            shell_hint_shown: false,
            auto_update: true,
        }
    }
}

pub use crate::platform::{config_dir, local_dir};

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join(crate::platform::HOOK_EXE)
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    crate::platform::ensure_private_dir(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}
