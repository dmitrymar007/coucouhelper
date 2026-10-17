//! Opt-in VS Code stdio bridge. Keep the official extension untouched; change
//! only its documented executable override after a diff and an explicit click.
use crate::{hooks, platform, settings};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
#[path = "../../hook/src/codex_binary.rs"]
mod codex_binary;

const KEY: &str = "chatgpt.cliExecutable";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    installed: bool,
    settings_path: String,
    launcher_path: String,
    codex_path: Option<String>,
    ready: bool,
}

fn settings_path() -> PathBuf {
    platform::config_dir()
        .parent()
        .unwrap_or(Path::new("."))
        .join("Code/User/settings.json")
}
fn launcher_path() -> PathBuf {
    settings::local_dir().join("bin/coucou-codex")
}

pub fn status() -> Status {
    let launcher = launcher_path();
    let codex = codex_binary::find(&platform::home_dir());
    let installed = read(&settings_path())
        .ok()
        .and_then(|s| parse(&s).ok())
        .and_then(|v| v.get(KEY)?.as_str().map(str::to_string))
        .is_some_and(|s| s == launcher.to_string_lossy());
    Status {
        installed,
        settings_path: settings_path().to_string_lossy().into(),
        launcher_path: launcher.to_string_lossy().into(),
        codex_path: codex.as_ref().map(|p| p.to_string_lossy().into()),
        ready: settings::hook_exe_path().is_file() && codex.is_some(),
    }
}

/// The launcher stays valid when the official extension updates its directory.
pub fn ensure_launcher() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let path = launcher_path();
    platform::ensure_private_dir(&settings::local_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = format!(
        "#!/bin/sh\n# Coucou Codex VS Code bridge\nexec {} --codex-app-server \"$@\"\n",
        hooks::sh_quote(&settings::hook_exe_path().to_string_lossy())
    );
    if std::fs::read_to_string(&path).ok().as_deref() == Some(&text) {
        return Ok(());
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    hooks::write_like(&temp, &path, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}

fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}\n".into()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Mask JSONC comments without changing offsets. Strings, including URLs and
/// escaped quotes, are untouched. Only whitespace outside strings is masked.
fn clean(text: &str) -> Result<Vec<u8>, String> {
    let mut out = text.as_bytes().to_vec();
    let mut i = if out.starts_with(&[0xef, 0xbb, 0xbf]) {
        out[..3].fill(b' ');
        3
    } else {
        0
    };
    while i < out.len() {
        if out[i] == b'"' {
            i += 1;
            loop {
                if i >= out.len() {
                    return Err("Unterminated JSON string".into());
                }
                match out[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if out.get(i..i + 2) == Some(b"//") {
            while i < out.len() && out[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if out.get(i..i + 2) == Some(b"/*") {
            out[i..i + 2].fill(b' ');
            i += 2;
            while i + 1 < out.len() && &out[i..i + 2] != b"*/" {
                if out[i] != b'\n' && out[i] != b'\r' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i + 1 >= out.len() {
                return Err("Unterminated JSON comment".into());
            }
            out[i..i + 2].fill(b' ');
            i += 2;
        } else {
            i += 1;
        }
    }
    // Trailing commas in VS Code JSONC are valid. Skip strings again.
    i = 0;
    while i < out.len() {
        if out[i] == b'"' {
            i += 1;
            while i < out.len() {
                match out[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else {
            if out[i] == b',' {
                let next = out[i + 1..]
                    .iter()
                    .copied()
                    .find(|b| !b.is_ascii_whitespace());
                if matches!(next, Some(b'}' | b']')) {
                    out[i] = b' ';
                }
            }
            i += 1;
        }
    }
    Ok(out)
}
fn parse(text: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(&clean(text)?)
        .map_err(|e| format!("VS Code settings are invalid: {e}"))?;
    if !v.is_object() {
        return Err("VS Code settings must be a JSON object".into());
    }
    Ok(v)
}

/// Replace only this property's scalar value, or insert one new top-level
/// property. Everything else survives byte for byte, including JSONC comments.
fn patched(text: &str, launcher: &str, install: bool) -> Result<String, String> {
    let current = parse(text)?;
    let previous = current.get(KEY);
    if let Some(value) = previous.filter(|v| !v.is_null()) {
        if value.as_str() != Some(launcher) {
            return Err("VS Code already uses a custom Codex executable. Coucou will not replace it. Remove that override yourself before enabling this bridge.".into());
        }
    }
    let replacement = if install {
        serde_json::to_string(launcher).unwrap()
    } else {
        "null".into()
    };
    let bytes = clean(text)?;
    let open = bytes.iter().position(|b| *b == b'{').unwrap();
    if previous.is_none() {
        if !install {
            return Ok(text.into());
        }
        let comma = if current.as_object().unwrap().is_empty() {
            ""
        } else {
            ","
        };
        let mut out = text.to_string();
        out.insert_str(open + 1, &format!("\n  \"{KEY}\": {replacement}{comma}"));
        return Ok(out);
    }
    let mut depth = 1;
    let mut range = None;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                if depth != 1 {
                    continue;
                }
                let key: String =
                    serde_json::from_slice(&bytes[start..i]).map_err(|e| e.to_string())?;
                let mut colon = i;
                while bytes[colon].is_ascii_whitespace() {
                    colon += 1;
                }
                if key != KEY || bytes[colon] != b':' {
                    continue;
                }
                let mut start = colon + 1;
                while bytes[start].is_ascii_whitespace() {
                    start += 1;
                }
                let mut end = start;
                if bytes[start] == b'"' {
                    end += 1;
                    while bytes[end] != b'"' {
                        end += if bytes[end] == b'\\' { 2 } else { 1 };
                    }
                    end += 1;
                } else {
                    while bytes.get(end).is_some_and(|b| b.is_ascii_alphabetic()) {
                        end += 1;
                    }
                }
                if range.is_some() {
                    return Err("Duplicate Codex executable settings. Remove the duplicate before enabling the bridge.".into());
                }
                range = Some(start..end);
                i = end;
            }
            _ => i += 1,
        }
    }
    let range = range.ok_or("Cannot locate Codex executable setting safely")?;
    let mut out = text.to_string();
    out.replace_range(range, &replacement);
    parse(&out)?;
    Ok(out)
}

pub fn preview(install: bool) -> Result<hooks::HookPreview, String> {
    preview_at(
        &settings_path(),
        &launcher_path().to_string_lossy(),
        install,
    )
}
fn preview_at(path: &Path, launcher: &str, install: bool) -> Result<hooks::HookPreview, String> {
    let current = read(path)?;
    let next = patched(&current, launcher, install)?;
    Ok(hooks::HookPreview {
        diff: hooks::unified_diff(&current, &next),
        backup: format!("{}.bak-coucou-codex-{}", path.display(), hooks::stamp()),
        settings_path: path.to_string_lossy().into(),
        fingerprint: hooks::fingerprint(current.as_bytes()),
    })
}

pub fn apply(install: bool, fingerprint: &str) -> Result<String, String> {
    if install {
        if !status().ready {
            return Err(
                "Install the official Codex VS Code extension and restart Coucou first.".into(),
            );
        }
        ensure_launcher()?;
    }
    apply_at(
        &settings_path(),
        &launcher_path().to_string_lossy(),
        install,
        fingerprint,
    )
}
fn apply_at(
    path: &Path,
    launcher: &str,
    install: bool,
    fingerprint: &str,
) -> Result<String, String> {
    let current = read(path)?;
    if hooks::fingerprint(current.as_bytes()) != fingerprint {
        return Err("VS Code settings changed since the preview. Review the new diff.".into());
    }
    let next = patched(&current, launcher, install)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let backup = format!(
        "{}.bak-coucou-codex-{}-{}",
        path.display(),
        hooks::stamp(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    );
    if path.exists() {
        std::fs::copy(path, &backup).map_err(|e| e.to_string())?;
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    let temp = target.with_extension(format!("json.coucou-{}", std::process::id()));
    hooks::write_like(&temp, &target, next.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(temp, target).map_err(|e| e.to_string())?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_only_the_top_level_property_and_preserves_jsonc() {
        let text = "\u{feff}{\n// keep this\n\"url\":\"https://example.com/*literal*/\",\n\"nested\":{\"chatgpt.cliExecutable\":\"leave\"},\n\"array\":[1,2,],\n}\n";
        let after = patched(text, "/home/it's me/coucou-codex", true).unwrap();
        assert!(after.contains(text.trim_start_matches('\u{feff}').trim_start_matches('{')));
        assert_eq!(parse(&after).unwrap()[KEY], "/home/it's me/coucou-codex");
        let removed = patched(&after, "/home/it's me/coucou-codex", false).unwrap();
        assert!(parse(&removed).unwrap()[KEY].is_null());
        assert_eq!(parse(&removed).unwrap()["nested"][KEY], "leave");
        assert!(patched(
            r#"{"chatgpt.cliExecutable":"/custom/codex"}"#,
            "/ours",
            true
        )
        .is_err());
        assert!(patched("{broken", "/ours", true).is_err());
        assert!(patched(
            r#"{"chatgpt.cliExecutable":null,"chatgpt.cliExecutable":null}"#,
            "/ours",
            true
        )
        .is_err());
    }
    #[test]
    fn writes_backup_and_refuses_a_stale_preview() {
        let dir = std::env::temp_dir().join(format!("coucou-vscode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let original = "{ // personal settings\n \"editor.fontSize\": 15,\n}\n";
        std::fs::write(&path, original).unwrap();
        let plan = preview_at(&path, "/bridge", true).unwrap();
        let backup = apply_at(&path, "/bridge", true, &plan.fingerprint).unwrap();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);
        let plan = preview_at(&path, "/bridge", false).unwrap();
        std::fs::write(&path, original).unwrap();
        assert!(apply_at(&path, "/bridge", false, &plan.fingerprint).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
