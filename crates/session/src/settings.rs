use std::path::Path;

use serde::{Deserialize, Serialize};

/// The Off/Word/Character setting from docs/UI/ui-02.png's mockup (M2 shipped Character only;
/// M4 adds the other two -- see DECISIONS.md). `Off` is a pure frontend concern (skip the
/// `intra_line_spans` IPC call entirely rather than fetching empty spans -- an IPC round-trip per
/// visible `Replace` line for a feature that's turned off would be wasted work), so `diff-core`
/// itself only ever sees `Word`/`Character`; this variant exists so the *setting* has a complete,
/// three-way shape to persist and expose in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntraLineMode {
    Off,
    Word,
    Character,
}

/// M5's "Default side when taking both" (docs/UI/ui-02.png's MERGE group) -- which side's content
/// comes first when a conflict hunk is resolved via `Resolution::TakeBoth` in `merge-core`. This
/// is a real merge-core input, not presentation: it decides the actual byte order of the merged
/// output, so it's threaded through to the `TakeBoth` resolution call, not just displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeBothSide {
    MineFirst,
    TheirsFirst,
}

/// Global preferences, per docs/PLAN.md §5 ("resolved settings (global + per-session override)").
/// This struct *is* the global half; the per-session override for `ignore_whitespace`/
/// `ignore_case` lives in the frontend's per-tab state (see `+page.svelte`'s `FileTab`), seeded
/// from these values when a tab opens but never written back here. `collapse_context_lines` and
/// `intra_line_mode` have no per-tab override -- they apply uniformly, matching the mockup (only
/// "Ignore whitespace"/"Ignore case" appear as toolbar quick-toggles; collapse-lines and
/// highlight-mode live only in the settings window).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub ignore_whitespace: bool,
    pub ignore_case: bool,
    pub collapse_context_lines: u32,
    pub intra_line_mode: IntraLineMode,
    /// Presentation-only, per DECISIONS.md: merge-core always classifies AutoMerged/Conflict
    /// honestly regardless of this toggle. It only controls whether the merge view surfaces
    /// AutoMerged hunks to the user as reviewable rows or hides them (auto-applies them
    /// silently), matching the mockup's "Only stop where both sides touched the same lines."
    pub auto_resolve_non_conflicting: bool,
    pub default_take_both_side: TakeBothSide,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ignore_whitespace: false,
            ignore_case: false,
            collapse_context_lines: 3,
            intra_line_mode: IntraLineMode::Character,
            auto_resolve_non_conflicting: true,
            default_take_both_side: TakeBothSide::MineFirst,
        }
    }
}

const SETTINGS_FILENAME: &str = "settings.json";

/// Loads settings from `<config_dir>/settings.json`. Falls back to `Settings::default()` --
/// silently, not as an error -- if the file is missing (first run) or fails to parse (a corrupt
/// or older-schema file); a preferences file that can't be read must never brick the app, and
/// there's no UI surface at startup to usefully report the failure to anyway.
pub fn load_settings(config_dir: &Path) -> Settings {
    std::fs::read_to_string(config_dir.join(SETTINGS_FILENAME))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Persists settings to `<config_dir>/settings.json`, creating the directory if it doesn't exist
/// yet (true on first run, before anything has ever been saved there).
pub fn save_settings(config_dir: &Path, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join(SETTINGS_FILENAME), json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("diffgrid-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn defaults_match_what_m1_m2_already_shipped() {
        let s = Settings::default();
        assert!(!s.ignore_whitespace);
        assert!(!s.ignore_case);
        assert_eq!(s.collapse_context_lines, 3, "matches diffView.ts's existing COLLAPSE_CONTEXT_LINES constant");
        assert_eq!(s.intra_line_mode, IntraLineMode::Character, "M1/M2 shipped Character-only, always on");
    }

    #[test]
    fn merge_defaults_match_ui_02_mockup() {
        let s = Settings::default();
        assert!(s.auto_resolve_non_conflicting, "mockup shows this toggle on by default");
        assert_eq!(s.default_take_both_side, TakeBothSide::MineFirst, "mockup shows \"Mine first\" selected by default");
    }

    #[test]
    fn take_both_side_serializes_as_lowercase_camel_case_variants() {
        assert_eq!(serde_json::to_value(TakeBothSide::MineFirst).unwrap(), "mineFirst");
        assert_eq!(serde_json::to_value(TakeBothSide::TheirsFirst).unwrap(), "theirsFirst");
    }

    #[test]
    fn settings_serialize_with_camel_case_field_names() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert!(json.get("ignoreWhitespace").is_some());
        assert!(json.get("collapseContextLines").is_some());
        assert!(json.get("intraLineMode").is_some());
    }

    #[test]
    fn intra_line_mode_serializes_as_lowercase_camel_case_variants() {
        assert_eq!(serde_json::to_value(IntraLineMode::Off).unwrap(), "off");
        assert_eq!(serde_json::to_value(IntraLineMode::Word).unwrap(), "word");
        assert_eq!(serde_json::to_value(IntraLineMode::Character).unwrap(), "character");
    }

    #[test]
    fn load_settings_returns_defaults_when_no_file_exists_yet() {
        let dir = temp_dir("load-missing");
        assert_eq!(load_settings(&dir), Settings::default());
    }

    #[test]
    fn load_settings_returns_defaults_rather_than_erroring_on_corrupt_json() {
        let dir = temp_dir("load-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_FILENAME), b"{ this is not valid json").unwrap();
        assert_eq!(load_settings(&dir), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_then_load_round_trips_exactly() {
        let dir = temp_dir("round-trip");
        let settings = Settings {
            ignore_whitespace: true,
            ignore_case: true,
            collapse_context_lines: 5,
            intra_line_mode: IntraLineMode::Word,
            auto_resolve_non_conflicting: false,
            default_take_both_side: TakeBothSide::TheirsFirst,
        };
        save_settings(&dir, &settings).unwrap();
        assert_eq!(load_settings(&dir), settings);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_settings_creates_the_config_dir_if_it_does_not_exist() {
        let dir = temp_dir("create-dir");
        assert!(!dir.exists());
        save_settings(&dir, &Settings::default()).unwrap();
        assert!(dir.join(SETTINGS_FILENAME).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_save_overwrites_the_first_rather_than_appending() {
        let dir = temp_dir("overwrite");
        save_settings(&dir, &Settings { ignore_whitespace: true, ..Settings::default() }).unwrap();
        save_settings(&dir, &Settings { ignore_whitespace: false, ..Settings::default() }).unwrap();
        assert!(!load_settings(&dir).ignore_whitespace);
        std::fs::remove_dir_all(&dir).ok();
    }
}
