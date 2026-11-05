//! 英語と日本語の表示文。製品名はそのままにする。

use crate::domain::adaptation::SuggestionDiff;
use crate::domain::language::Language;

pub fn menu_apply_with_diff(language: Language, diff: &str) -> String {
    match language {
        Language::Ja => format!("★ 感度の微調整を適用: {diff}"),
        Language::En => format!("★ Apply Sensitivity Adjustment: {diff}"),
    }
}

pub fn menu_dismiss_suggestion(language: Language) -> &'static str {
    match language {
        Language::Ja => "提案を見送る",
        Language::En => "Dismiss Suggestion",
    }
}

pub fn menu_calibrate(language: Language) -> &'static str {
    match language {
        Language::Ja => "設定 / キャリブレーション...",
        Language::En => "Settings / Calibration...",
    }
}

pub fn menu_startup(language: Language) -> &'static str {
    match language {
        Language::Ja => "Windows 起動時に実行",
        Language::En => "Run at Windows Startup",
    }
}

pub fn menu_show_background(language: Language) -> &'static str {
    match language {
        Language::Ja => "デスクトップ背景表示",
        Language::En => "Show Desktop Background",
    }
}

pub fn menu_pause(language: Language) -> &'static str {
    match language {
        Language::Ja => "サークル検知を一時停止",
        Language::En => "Pause Circle Detection",
    }
}

pub fn menu_exit(language: Language) -> &'static str {
    match language {
        Language::Ja => "終了",
        Language::En => "Exit",
    }
}

pub fn menu_language() -> &'static str {
    "Language / 言語"
}

pub fn menu_language_auto() -> &'static str {
    "Auto (System)"
}

pub fn menu_language_en() -> &'static str {
    "English"
}

pub fn menu_language_ja() -> &'static str {
    "日本語"
}

pub fn badge_apply(language: Language) -> &'static str {
    match language {
        Language::Ja => "感度の微調整を適用",
        Language::En => "Apply Sensitivity Adjustment",
    }
}

pub fn badge_dismiss(language: Language) -> &'static str {
    match language {
        Language::Ja => "今回は見送る",
        Language::En => "Dismiss for Now",
    }
}

pub fn tip_active() -> &'static str {
    "Photon Pointer Warp"
}

pub fn tip_paused(language: Language) -> &'static str {
    match language {
        Language::Ja => "Photon Pointer Warp (一時停止中)",
        Language::En => "Photon Pointer Warp (Paused)",
    }
}

pub fn tip_suggest(language: Language) -> &'static str {
    match language {
        Language::Ja => "Photon Pointer Warp (感度微調整の提案あり)",
        Language::En => "Photon Pointer Warp (Sensitivity adjustment suggested)",
    }
}

pub fn format_diffs(language: Language, diffs: &[SuggestionDiff]) -> String {
    diffs
        .iter()
        .map(|diff| format_diff(language, *diff))
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_diff(language: Language, diff: SuggestionDiff) -> String {
    match (language, diff) {
        (Language::Ja, SuggestionDiff::WindowMs(ms)) => format!("窓 {ms:+}ms"),
        (Language::En, SuggestionDiff::WindowMs(ms)) => format!("Window {ms:+}ms"),
        (Language::Ja, SuggestionDiff::AngleDeg(deg)) => format!("角 {deg:+}°"),
        (Language::En, SuggestionDiff::AngleDeg(deg)) => format!("Angle {deg:+}°"),
        (Language::Ja, SuggestionDiff::RelaxClosure(value)) => {
            format!("閉合緩和 {value:+.2}")
        }
        (Language::En, SuggestionDiff::RelaxClosure(value)) => {
            format!("Relax closure {value:+.2}")
        }
        (Language::Ja, SuggestionDiff::ThumbPartialClosure) => "親指部分閉合".to_string(),
        (Language::En, SuggestionDiff::ThumbPartialClosure) => "Thumb partial closure".to_string(),
        (Language::Ja, SuggestionDiff::Eccentricity(value)) => format!("楕円 {value:+.1}"),
        (Language::En, SuggestionDiff::Eccentricity(value)) => {
            format!("Eccentricity {value:+.1}")
        }
    }
}

pub fn calibrate_reset(language: Language) -> &'static str {
    match language {
        Language::Ja => "やり直す",
        Language::En => "Reset",
    }
}

pub fn calibrate_save(language: Language) -> &'static str {
    match language {
        Language::Ja => "保存",
        Language::En => "Save",
    }
}

pub fn calibrate_cancel(language: Language) -> &'static str {
    match language {
        Language::Ja => "キャンセル",
        Language::En => "Cancel",
    }
}

pub fn calibrate_guide(language: Language) -> &'static str {
    match language {
        Language::Ja => "キャンバス内で止まって枠がシアンになったら、普段の感覚で回してください",
        Language::En => {
            "Pause inside the canvas until the frame turns cyan, then draw circles the way you usually do"
        }
    }
}

pub fn calibrate_ready(language: Language) -> &'static str {
    match language {
        Language::Ja => "Ready: 自由な場所・向きで回してください",
        Language::En => "Ready: draw in any place and direction",
    }
}

pub fn calibrate_samples(language: Language, count: usize) -> String {
    match language {
        Language::Ja => format!("Samples: {count} 回記録済み（納得いくまで回せます）"),
        Language::En => format!("Samples: {count} recorded (draw until you are satisfied)"),
    }
}

pub fn calibrate_suggested(language: Language, window_ms: u64, angle_deg: f32, eccentricity: f32) -> String {
    match language {
        Language::Ja => format!(
            "推奨設定: 窓 {window_ms}ms / 角 {angle_deg:.0}° / 楕円比 {eccentricity:.1}"
        ),
        Language::En => format!(
            "Suggested: window {window_ms}ms / angle {angle_deg:.0}° / eccentricity {eccentricity:.1}"
        ),
    }
}

pub fn calibrate_suggest_failed(language: Language, reason: &str) -> String {
    let detail = calibrate_failure(language, reason);
    match language {
        Language::Ja => format!("推奨を出せません: {detail}"),
        Language::En => format!("Cannot suggest: {detail}"),
    }
}

pub fn calibrate_suggest_need_more(language: Language) -> &'static str {
    match language {
        Language::Ja => "推奨設定: 3 回以上描くとここに表示されます",
        Language::En => "Suggested settings appear after 3 or more strokes",
    }
}

pub fn calibrate_suggest_calculating(language: Language) -> &'static str {
    match language {
        Language::Ja => "推奨設定: 算出中",
        Language::En => "Suggested settings: calculating",
    }
}

pub fn calibrate_failure(language: Language, reason: &str) -> &'static str {
    match (language, reason) {
        (Language::Ja, "mixed_direction") => "回転方向が混在しています",
        (Language::En, "mixed_direction") => "Directions are mixed",
        (Language::Ja, "variance_too_high") => "ばらつきが大きすぎます。やり直してください",
        (Language::En, "variance_too_high") => "The strokes vary too much. Try again",
        (Language::Ja, "too_few_after_outliers") => "有効なサンプルが不足しています",
        (Language::En, "too_few_after_outliers") => "Not enough valid samples",
        (Language::Ja, "too_few_strokes") => "サンプルが不足しています",
        (Language::En, "too_few_strokes") => "Not enough samples",
        (Language::Ja, "too_many_strokes") => "サンプルが多すぎます",
        (Language::En, "too_many_strokes") => "Too many samples",
        (Language::Ja, _) => "条件を満たしません",
        (Language::En, _) => "The strokes do not meet the conditions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_diffs_match_the_previous_menu_text() {
        let diffs = [
            SuggestionDiff::WindowMs(40),
            SuggestionDiff::AngleDeg(-10),
            SuggestionDiff::RelaxClosure(0.05),
            SuggestionDiff::ThumbPartialClosure,
            SuggestionDiff::Eccentricity(0.2),
        ];
        assert_eq!(
            format_diffs(Language::Ja, &diffs),
            "窓 +40ms / 角 -10° / 閉合緩和 +0.05 / 親指部分閉合 / 楕円 +0.2"
        );
        assert_eq!(
            format_diffs(Language::En, &diffs),
            "Window +40ms / Angle -10° / Relax closure +0.05 / Thumb partial closure / Eccentricity +0.2"
        );
    }

    #[test]
    fn tray_and_badge_labels_differ_by_language() {
        assert_eq!(menu_pause(Language::Ja), "サークル検知を一時停止");
        assert_eq!(menu_pause(Language::En), "Pause Circle Detection");
        assert_eq!(badge_dismiss(Language::Ja), "今回は見送る");
        assert_eq!(badge_dismiss(Language::En), "Dismiss for Now");
        assert_eq!(menu_language(), "Language / 言語");
        assert_eq!(menu_language_auto(), "Auto (System)");
        assert_eq!(menu_language_ja(), "日本語");
        assert_eq!(tip_active(), "Photon Pointer Warp");
        assert_eq!(tip_paused(Language::En), "Photon Pointer Warp (Paused)");
        assert_eq!(calibrate_reset(Language::Ja), "やり直す");
        assert_eq!(calibrate_reset(Language::En), "Reset");
        assert_eq!(calibrate_save(Language::Ja), "保存");
        assert_eq!(calibrate_save(Language::En), "Save");
        assert_eq!(calibrate_cancel(Language::Ja), "キャンセル");
        assert_eq!(calibrate_cancel(Language::En), "Cancel");
        assert_eq!(
            calibrate_failure(Language::En, "mixed_direction"),
            "Directions are mixed"
        );
        assert_eq!(
            calibrate_suggest_failed(Language::Ja, "too_few_strokes"),
            "推奨を出せません: サンプルが不足しています"
        );
    }
}
