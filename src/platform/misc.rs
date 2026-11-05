//! OS の UI 言語 ID。ドメインには渡すだけで、ここでだけ Win32 を呼ぶ。

/// `GetUserDefaultUILanguage` の LANGID。Windows 以外では en-US (`0x0409`)。
pub fn get_system_ui_language() -> u16 {
    #[cfg(windows)]
    {
        unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() }
    }
    #[cfg(not(windows))]
    {
        0x0409
    }
}
