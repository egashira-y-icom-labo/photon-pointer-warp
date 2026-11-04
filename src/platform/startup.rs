//! HKCU Run へのスタートアップ登録（管理者権限不要）

use std::io;
use std::path::Path;

use windows::core::w;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, REG_VALUE_TYPE,
};

const RUN_SUBKEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: windows::core::PCWSTR = w!("PhotonPointerWarp");

struct OpenKey(HKEY);

impl Drop for OpenKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_run_key(access: u32) -> io::Result<OpenKey> {
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            0,
            windows::Win32::System::Registry::REG_SAM_FLAGS(access),
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(reg_error(status, "RegOpenKeyExW"));
    }
    Ok(OpenKey(hkey))
}

fn reg_error(status: WIN32_ERROR, api: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        format!("{api}: {}", io::Error::from_raw_os_error(status.0 as i32)),
    )
}

fn quote_exe_command(path: &Path) -> String {
    let raw = strip_extended_prefix(&path.to_string_lossy());
    format!("\"{raw}\"")
}

fn strip_extended_prefix(s: &str) -> String {
    s.strip_prefix(r"\\?\").unwrap_or(s).to_string()
}

fn unquote_reg_sz(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_exe_path(s: &str) -> String {
    strip_extended_prefix(&unquote_reg_sz(s))
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn paths_match(registered: &str, current: &Path) -> bool {
    let left = normalize_exe_path(registered);
    let right = normalize_exe_path(&current.to_string_lossy());
    !left.is_empty() && left == right
}

fn utf16_le_sz(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn utf16_le_to_string(buf: &[u8]) -> String {
    let words: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16_lossy(&words)
}

fn query_startup_command(key: HKEY) -> Option<String> {
    let mut ty = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    let status =
        unsafe { RegQueryValueExW(key, VALUE_NAME, None, Some(&mut ty), None, Some(&mut size)) };
    if status != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    if ty != REG_SZ {
        return None;
    }
    let used = (size as usize).min(buf.len());
    Some(utf16_le_to_string(&buf[..used]))
}

/// `HKCU\...\Run\PhotonPointerWarp` が現在の実行ファイルを指しているか。
pub fn is_startup_enabled() -> bool {
    let Ok(key) = open_run_key(KEY_QUERY_VALUE.0) else {
        return false;
    };
    let Some(registered) = query_startup_command(key.0) else {
        return false;
    };
    match std::env::current_exe() {
        Ok(exe) => paths_match(&registered, &exe),
        Err(_) => !registered.trim().is_empty(),
    }
}

pub fn set_startup_enabled(enabled: bool) -> io::Result<()> {
    if enabled {
        let exe = std::env::current_exe()?;
        let command = quote_exe_command(&exe);
        let data = utf16_le_sz(&command);
        let key = open_run_key(KEY_SET_VALUE.0)?;
        let status = unsafe { RegSetValueExW(key.0, VALUE_NAME, 0, REG_SZ, Some(&data)) };
        if status != ERROR_SUCCESS {
            return Err(reg_error(status, "RegSetValueExW"));
        }
        Ok(())
    } else {
        let key = open_run_key(KEY_SET_VALUE.0)?;
        let status = unsafe { RegDeleteValueW(key.0, VALUE_NAME) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(reg_error(status, "RegDeleteValueW"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn quote_wraps_and_strips_extended_prefix() {
        let path = PathBuf::from(r"\\?\C:\Tools\photon-pointer-warp.exe");
        assert_eq!(
            quote_exe_command(&path),
            r#""C:\Tools\photon-pointer-warp.exe""#
        );
    }

    #[test]
    fn paths_match_ignores_quotes_and_case() {
        let current = PathBuf::from(r"C:\Tools\photon-pointer-warp.exe");
        assert!(paths_match(
            r#""c:\tools\photon-pointer-warp.exe""#,
            &current
        ));
        assert!(paths_match(r"C:\Tools\photon-pointer-warp.exe", &current));
        assert!(!paths_match(
            r#""C:\Other\photon-pointer-warp.exe""#,
            &current
        ));
    }
}
