//! Locate the official Codex binary without modifying the installed extension.
use std::path::{Path, PathBuf};

pub fn find(home: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for root in [
        ".vscode/extensions",
        ".vscode-insiders/extensions",
        ".vscode-oss/extensions",
    ] {
        let Ok(entries) = std::fs::read_dir(home.join(root)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix("openai.chatgpt-") else {
                continue;
            };
            let version: Vec<u64> = rest
                .split('-')
                .next()
                .unwrap_or("")
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();
            if version.len() != 3 {
                continue;
            }
            let arch = if cfg!(target_arch = "aarch64") {
                "linux-aarch64"
            } else {
                "linux-x86_64"
            };
            let path = entry.path().join("bin").join(arch).join("codex");
            if executable(&path) {
                candidates.push((version, path));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extension_updates_use_the_newest_executable_version() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("coucou-codex-binary-{}", std::process::id()));
        let arch = if cfg!(target_arch = "aarch64") {
            "linux-aarch64"
        } else {
            "linux-x86_64"
        };
        for version in ["0.99.0", "0.100.0"] {
            let path = root.join(format!(
                ".vscode/extensions/openai.chatgpt-{version}-linux-x64/bin/{arch}/codex"
            ));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(find(&root).unwrap().to_string_lossy().contains("0.100.0"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
