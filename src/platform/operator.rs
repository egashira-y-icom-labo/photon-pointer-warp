//! Photon Operator（`photon-ui.exe`）の起動パス解決

use std::path::{Path, PathBuf};

pub const OPERATOR_EXE_ENV: &str = "PHOTON_OPERATOR_EXE";
pub const OPERATOR_EXE_NAME: &str = "photon-ui.exe";

const DEV_RELEASE: &str = r"C:\projects\photon-operator\target\release\photon-ui.exe";
const DEV_DEBUG: &str = r"C:\projects\photon-operator\target\debug\photon-ui.exe";

/// 解決順: env → `current_exe` 隣接 → 開発用 release → debug。
pub fn resolve_operator_exe(
    env_value: Option<&str>,
    current_exe: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(value) = env_value.map(str::trim).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        if exists(&path) {
            return Some(path);
        }
    }
    if let Some(exe) = current_exe {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(OPERATOR_EXE_NAME);
            if exists(&sibling) {
                return Some(sibling);
            }
        }
    }
    for candidate in [DEV_RELEASE, DEV_DEBUG] {
        let path = PathBuf::from(candidate);
        if exists(&path) {
            return Some(path);
        }
    }
    None
}

/// 実行時の既定解決（ファイル存在チェック付き）。
pub fn resolve_operator_exe_default() -> Option<PathBuf> {
    resolve_operator_exe(
        std::env::var(OPERATOR_EXE_ENV).ok().as_deref(),
        std::env::current_exe().ok().as_deref(),
        |p| p.is_file(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn exists_in(set: &HashSet<PathBuf>) -> impl Fn(&Path) -> bool + '_ {
        move |p| set.contains(p)
    }

    #[test]
    fn prefers_env_when_present() {
        // テストは Linux 上でも parent()/join が動くよう `/` 区切りを使う。
        let env = PathBuf::from("D:/tools/photon-ui.exe");
        let sibling_dir = PathBuf::from("C:/app");
        let current = sibling_dir.join("photon-pointer-warp.exe");
        let mut files = HashSet::new();
        files.insert(env.clone());
        files.insert(sibling_dir.join(OPERATOR_EXE_NAME));
        files.insert(PathBuf::from(DEV_RELEASE));

        let got = resolve_operator_exe(
            Some(env.to_str().unwrap()),
            Some(&current),
            exists_in(&files),
        );
        assert_eq!(got, Some(env));
    }

    #[test]
    fn falls_back_to_sibling_of_current_exe() {
        let current = PathBuf::from("C:/app/photon-pointer-warp.exe");
        let sibling = PathBuf::from("C:/app/photon-ui.exe");
        let mut files = HashSet::new();
        files.insert(sibling.clone());
        files.insert(PathBuf::from(DEV_RELEASE));

        let got = resolve_operator_exe(None, Some(&current), exists_in(&files));
        assert_eq!(got, Some(sibling));
    }

    #[test]
    fn falls_back_to_dev_release_then_debug() {
        let mut files = HashSet::new();
        files.insert(PathBuf::from(DEV_DEBUG));
        let got = resolve_operator_exe(None, None, exists_in(&files));
        assert_eq!(got, Some(PathBuf::from(DEV_DEBUG)));

        files.insert(PathBuf::from(DEV_RELEASE));
        let got = resolve_operator_exe(None, None, exists_in(&files));
        assert_eq!(got, Some(PathBuf::from(DEV_RELEASE)));
    }

    #[test]
    fn empty_env_is_ignored() {
        let mut files = HashSet::new();
        files.insert(PathBuf::from(DEV_RELEASE));
        let got = resolve_operator_exe(Some("  "), None, exists_in(&files));
        assert_eq!(got, Some(PathBuf::from(DEV_RELEASE)));
    }

    #[test]
    fn none_when_nothing_exists() {
        let files = HashSet::new();
        let got = resolve_operator_exe(
            Some("D:/missing/photon-ui.exe"),
            Some(Path::new("C:/app/photon-pointer-warp.exe")),
            exists_in(&files),
        );
        assert!(got.is_none());
    }
}
