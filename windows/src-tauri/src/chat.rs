// The island's chat, whichever way it reaches Claude: through Claude Code and
// the user's subscription (the default), or straight to the API with a key.
// Commands talk to `Chat` only and never learn which one answered.

use std::sync::Arc;
use std::time::Duration;

use crate::claude::{self, ChatContext, ChatReply};
use crate::claude_cli::{self, CliChat, CliStatus, StreamUpdate, TranscriptMessage};
use crate::settings::Settings;

/// Values of `Settings::chat_backend`.
pub const BACKEND_CLAUDE_CODE: &str = "claude-code";
pub const BACKEND_API_KEY: &str = "api-key";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    ClaudeCode,
    ApiKey,
}

impl Backend {
    /// Anything unknown is the default, Claude Code.
    pub fn from_setting(value: &str) -> Self {
        if value == BACKEND_API_KEY {
            Backend::ApiKey
        } else {
            Backend::ClaudeCode
        }
    }
}

/// What a turn needs from the preferences, copied out so no lock is held
/// across the answer.
pub struct TurnSettings {
    backend: Backend,
    api_model: String,
    cli_model: String,
    idle: Duration,
}

impl From<&Settings> for TurnSettings {
    fn from(s: &Settings) -> Self {
        Self {
            backend: Backend::from_setting(&s.chat_backend),
            api_model: s.model.clone(),
            cli_model: s.cli_model.clone(),
            idle: Duration::from_secs(u64::from(s.chat_idle_minutes) * 60),
        }
    }
}

pub struct Sent {
    pub reply: ChatReply,
    /// The Claude Code session, to offer it again after a restart.
    pub session: Option<String>,
}

#[derive(Default)]
pub struct Chat {
    api: claude::Chat,
    cli: Arc<CliChat>,
}

impl Chat {
    /// One turn. `on_update` gets the answer as it streams in (Claude Code
    /// only; the API answer arrives in one piece).
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
        }
    }

    /// A new conversation on both backends.
    pub fn reset(&self) {
        self.api.reset();
        self.cli.reset();
    }

    /// Ends the Claude Code process; the conversation goes on with the next
    /// message.
    pub fn stop(&self) {
        self.cli.stop();
    }

    pub fn status(&self) -> CliStatus {
        self.cli.status()
    }

    /// Picks up a saved Claude Code conversation. The API backend has no
    /// saved conversations, so it starts afresh.
    pub fn resume(&self, session: &str) -> Result<Vec<TranscriptMessage>, String> {
        self.api.reset();
        self.cli.resume(session)
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
        assert_eq!(Backend::from_setting("opencode"), Backend::ClaudeCode);
    }
}
