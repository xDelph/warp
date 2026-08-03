use std::ffi::OsString;
use std::path::{Path, PathBuf};

use warp_cli::agent::Harness;

pub(crate) fn augmented_path() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    for path in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/opt/local/bin",
    ] {
        let path = PathBuf::from(path);
        if !paths.iter().any(|candidate| candidate == &path) {
            paths.push(path);
        }
    }
    for path in user_bin_paths() {
        if !paths.iter().any(|candidate| candidate == &path) {
            paths.push(path);
        }
    }
    paths
}

pub(crate) fn augmented_path_env() -> OsString {
    std::env::join_paths(augmented_path()).unwrap_or_default()
}

pub(crate) fn resolve_command(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return command_path.exists().then(|| command_path.to_path_buf());
    }

    augmented_path()
        .into_iter()
        .map(|path| path.join(command))
        .find(|path| path.is_file())
}

pub(crate) fn resolve_harness_command(harness: Harness, command: &str) -> Option<PathBuf> {
    if harness == Harness::Codex {
        return resolve_codex_acp_command().or_else(|| {
            resolve_command(command).filter(|path| !is_deprecated_zed_codex_acp_path(path))
        });
    }

    resolve_command(command)
}

fn resolve_codex_acp_command() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_ACP_BIN")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }

    first_usable_codex_acp_path(known_zed_codex_acp_paths()).or_else(|| {
        first_usable_codex_acp_path(
            augmented_path()
                .into_iter()
                .map(|path| path.join("codex-acp")),
        )
    })
}

/// The deprecated `@zed-industries/codex-acp` package is a stub that exits
/// immediately with code 1, so any resolution path — including the "known"
/// install locations — must skip it or model discovery and prompting both
/// break. The official package is `@agentclientprotocol/codex-acp`.
fn first_usable_codex_acp_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths
        .into_iter()
        .find(|path| path.is_file() && !is_deprecated_zed_codex_acp_path(path))
}

fn known_zed_codex_acp_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };

    vec![
        home.join(".codex")
            .join("skills")
            .join("fast-agent")
            .join("node_modules")
            .join(".bin")
            .join("codex-acp"),
        home.join("node_modules").join(".bin").join("codex-acp"),
    ]
}

fn is_deprecated_zed_codex_acp_path(path: &Path) -> bool {
    std::fs::read_link(path)
        .or_else(|_| path.canonicalize())
        .map(|target| {
            target
                .to_string_lossy()
                .contains("@zed-industries/codex-acp")
        })
        .unwrap_or(false)
}

fn user_bin_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };

    let mut paths = vec![
        home.join(".local/bin"),
        home.join("bin"),
        home.join(".cargo/bin"),
        home.join(".bun/bin"),
        home.join(".deno/bin"),
        home.join(".npm/bin"),
        home.join(".npm-global/bin"),
        home.join(".yarn/bin"),
        home.join(".config/yarn/global/node_modules/.bin"),
        home.join(".local/share/pnpm"),
        home.join("Library/pnpm"),
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
    ];

    paths.extend(version_manager_bin_paths(&home.join(".nvm/versions/node")));
    paths.extend(version_manager_bin_paths(&home.join(".fnm/node-versions")));

    paths
}

fn version_manager_bin_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{first_usable_codex_acp_path, is_deprecated_zed_codex_acp_path};

    fn codex_acp_symlink(temp: &tempfile::TempDir, package: &str, link_name: &str) -> std::path::PathBuf {
        let target = temp
            .path()
            .join("node_modules")
            .join(package)
            .join("codex-acp")
            .join("dist")
            .join("index.js");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "").unwrap();
        let link = temp.path().join(link_name);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        link
    }

    #[test]
    fn resolution_skips_deprecated_zed_package() {
        let temp = tempfile::tempdir().unwrap();
        let deprecated_link = codex_acp_symlink(&temp, "@zed-industries", "deprecated-codex-acp");

        let good = temp.path().join("codex-acp");
        std::fs::write(&good, "").unwrap();

        assert_eq!(
            first_usable_codex_acp_path([deprecated_link.clone(), good.clone()]),
            Some(good)
        );
        assert_eq!(first_usable_codex_acp_path([deprecated_link]), None);
    }

    #[test]
    fn deprecated_zed_codex_acp_symlink_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let link = codex_acp_symlink(&temp, "@zed-industries", "codex-acp");

        assert!(is_deprecated_zed_codex_acp_path(&link));
    }

    #[test]
    fn official_agentclientprotocol_package_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let link = codex_acp_symlink(&temp, "@agentclientprotocol", "codex-acp");

        assert!(!is_deprecated_zed_codex_acp_path(&link));
        assert_eq!(first_usable_codex_acp_path([link.clone()]), Some(link));
    }
}
