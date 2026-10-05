// The island's chat, whichever way it gets its answers: through Claude Code
// and the user's subscription (the default), through opencode and the
// providers set up in it, or straight to the Anthropic API with a key.
// Commands talk to `Chat` only and never learn which one answered.

use std::sync::Arc;
use std::time::Duration;

use crate::claude::{self, ChatContext, ChatReply};
use crate::claude_cli::{self, CliChat, CliStatus, SessionInfo, StreamUpdate, TranscriptMessage};
use crate::opencode_cli::{self, OpenCodeChat};
use crate::settings::Settings;

/// Values of `Settings::chat_backend`.
pub const BACKEND_CLAUDE_CODE: &str = "claude-code";
pub const BACKEND_API_KEY: &str = "api-key";
pub const BACKEND_OPENCODE: &str = "opencode";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    ClaudeCode,
    OpenCode,
    ApiKey,
}

impl Backend {
    /// Anything unknown is the default, Claude Code.
    pub fn from_setting(value: &str) -> Self {
        match value {
            BACKEND_API_KEY => Backend::ApiKey,
            BACKEND_OPENCODE => Backend::OpenCode,
            _ => Backend::ClaudeCode,
        }
    }
}

/// What a turn needs from the preferences, copied out so no lock is held
/// across the answer.
pub struct TurnSettings {
    backend: Backend,
    api_model: String,
    cli_model: String,
    opencode_model: String,
    idle: Duration,
}

impl From<&Settings> for TurnSettings {
    fn from(s: &Settings) -> Self {
        Self {
            backend: Backend::from_setting(&s.chat_backend),
            api_model: s.model.clone(),
            cli_model: s.cli_model.clone(),
            opencode_model: s.opencode_model.clone(),
            idle: Duration::from_secs(u64::from(s.chat_idle_minutes) * 60),
        }
    }
}

pub struct Sent {
    pub reply: ChatReply,
    /// The Claude Code or opencode session, to offer it again after a restart.
    pub session: Option<String>,
}

#[derive(Default)]
pub struct Chat {
    api: claude::Chat,
    cli: Arc<CliChat>,
    opencode: Arc<OpenCodeChat>,
}

impl Chat {
    /// One turn. `on_update` gets the answer as it streams in (Claude Code
    /// and opencode; the API answer arrives in one piece).
    pub async fn send(
        &self,
        settings: TurnSettings,
        query: String,
        context: Option<ChatContext>,
        on_update: impl Fn(StreamUpdate),
    ) -> Result<Sent, String> {
        match settings.backend {
            Backend::ApiKey => {
                let reply = claude::send(&self.api, &settings.api_model, query, context).await?;
                Ok(Sent { reply, session: None })
            }
            Backend::ClaudeCode => {
                let result = self.cli.send(&settings.cli_model, query, context, on_update).await;
                self.cli.stop_when_idle(settings.idle);
                let (reply, session) = result?;
                Ok(Sent { reply, session: Some(session) })
            }
            Backend::OpenCode => {
                let result = self.opencode.send(&settings.opencode_model, query, context, on_update).await;
                self.opencode.stop_when_idle(settings.idle);
                let (reply, session) = result?;
                Ok(Sent { reply, session: (!session.is_empty()).then_some(session) })
            }
        }
    }

    /// A new conversation on every backend.
    pub fn reset(&self) {
        self.api.reset();
        self.cli.reset();
        self.opencode.reset();
    }

    /// Ends the Claude Code process and any opencode answer being written; the
    /// conversation goes on with the next message.
    pub fn stop(&self) {
        self.cli.stop();
        self.opencode.stop();
    }

    pub fn status(&self, backend: Backend) -> CliStatus {
        match backend {
            Backend::OpenCode => self.opencode.status(),
            _ => self.cli.status(),
        }
    }

    /// Picks up a saved conversation; the id says whose it is. The API
    /// backend has no saved conversations, so it starts afresh.
    pub async fn resume(&self, session: &str) -> Result<Vec<TranscriptMessage>, String> {
        self.api.reset();
        if opencode_cli::is_session_id(session) {
            self.opencode.resume(session).await
        } else {
            self.cli.resume(session)
        }
    }

    /// Recent conversations started from the island, on this backend.
    pub async fn sessions(&self, backend: Backend, limit: usize) -> Vec<SessionInfo> {
        match backend {
            Backend::OpenCode => opencode_cli::list_sessions(limit).await,
            _ => claude_cli::list_sessions(limit),
        }
    }
}

pub use claude_cli::install_status;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_backends_fall_back_to_claude_code() {
        assert_eq!(Backend::from_setting(BACKEND_API_KEY), Backend::ApiKey);
        assert_eq!(Backend::from_setting(BACKEND_CLAUDE_CODE), Backend::ClaudeCode);
        assert_eq!(Backend::from_setting(""), Backend::ClaudeCode);
        assert_eq!(Backend::from_setting(BACKEND_OPENCODE), Backend::OpenCode);
        assert_eq!(Backend::from_setting("gemini"), Backend::ClaudeCode);
    }
}
