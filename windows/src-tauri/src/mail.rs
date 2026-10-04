// Sending a dropped file by email — the port of MailView.sendMail() in
// IslandViewContent.swift. Only ever runs from the Send button.
//
// With Resend configured (API key and sender address) the email goes out
// through Resend, as on macOS. Otherwise the user's own mail app opens with
// the email written and the file attached, and the user sends it from there.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::{files, platform, secrets};

/// Resend accepts 40 MB per email, base64 included.
const MAX_ATTACHMENT: u64 = 25 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Sent {
    /// Gone, through Resend.
    Resend,
    /// Written in the user's mail app, waiting for them to send it.
    MailApp,
}

/// One plain address: no spaces, no line breaks, nothing that a command line
/// or a header could take for something else.
pub fn valid_address(to: &str) -> bool {
    let to = to.trim();
    let Some((local, domain)) = to.split_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && to.len() <= 254
        && !to.starts_with('-')
        && to.chars().all(|c| !c.is_whitespace() && !c.is_control() && !"<>,;:\"'()[]\\".contains(c))
}

/// Only files Coucou itself copied into its inbox may be attached.
fn inbox_file(path: &str) -> Result<PathBuf, String> {
    let inbox = files::inbox_dir().canonicalize().map_err(|e| e.to_string())?;
    let file = Path::new(path).canonicalize().map_err(|_| "The file is gone.".to_string())?;
    if !file.starts_with(&inbox) || !file.is_file() {
        return Err("Only a file dropped on the island can be attached.".into());
    }
    Ok(file)
}

pub async fn send(to: String, subject: String, body: String, file: Option<String>) -> Result<Sent, String> {
    let to = to.trim().to_string();
    if !valid_address(&to) {
        return Err("Missing or invalid recipient.".into());
    }
    let attachment = file.as_deref().map(inbox_file).transpose()?;
    let subject = subject.trim().to_string();
    let subject = if subject.is_empty() {
        attachment
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "File".into())
    } else {
        subject
    };

    match (secrets::get("resend-api-key"), secrets::get("resend-from")) {
        (Some(key), Some(from)) => {
            via_resend(&key, &from, &to, &subject, &body, attachment.as_deref()).await?;
            crate::log::line("mail: sent through Resend");
            Ok(Sent::Resend)
        }
        // A key but no sender: say so rather than quietly using the mail app.
        (Some(_), None) => Err("Set the sender address in Settings → Resend.".into()),
        _ => {
            platform::compose_email(&to, &subject, &body, attachment.as_deref())?;
            crate::log::line("mail: handed to the mail app");
            Ok(Sent::MailApp)
        }
    }
}

async fn via_resend(key: &str, from: &str, to: &str, subject: &str, body: &str, file: Option<&Path>) -> Result<(), String> {
    let mut payload = json!({
        "from": from,
        "to": [to],
        "subject": subject,
        "text": if body.is_empty() { " " } else { body },
    });
    if let Some(path) = file {
        let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if len > MAX_ATTACHMENT {
            return Err("The file is too large to send by email (25 MB at most).".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        payload["attachments"] = json!([{ "filename": name, "content": crate::claude::base64_for(&bytes) }]);
    }
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?
        .post("https://api.resend.com/emails")
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("Resend error ({}) — check the API key and the sender address.", response.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::valid_address;

    #[test]
    fn only_plain_addresses_pass() {
        for ok in ["a@b.co", "first.last+tag@example.org", "  x@y.fr  "] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in [
            "", "plain", "@b.co", "a@", "a@b", "a@.co", "a@b.co.", "a b@c.co", "a@b.co\nBcc: x@y.z",
            "-x@y.co", "a@b.co,c@d.co", "<a@b.co>", "a@b.co;rm",
        ] {
            assert!(!valid_address(bad), "{bad:?}");
        }
    }
}
