//! Shared layout preferences; viewport and browser drag events are adapter inputs.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const LAYOUT_PREFERENCES_KEY: &str = "workspace_layout";
pub const PHONE_BREAKPOINT: f64 = 800.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    Automatic,
    Desktop,
    Phone,
}
impl LayoutMode {
    pub fn phone(self, width: f64) -> bool {
        match self {
            Self::Automatic => width.is_finite() && (0.0..PHONE_BREAKPOINT).contains(&width),
            Self::Desktop => false,
            Self::Phone => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSide {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesView {
    #[default]
    Explorer,
    Changes,
    Search,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginPanelLayout {
    pub visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutPreferences {
    pub mode: LayoutMode,
    pub files_view: FilesView,
    pub include_hidden: bool,
    pub plugin_panels: BTreeMap<String, PluginPanelLayout>,
    sides: BTreeMap<String, PanelSide>,
    order: Vec<String>,
}
impl LayoutPreferences {
    pub fn side(&self, panel: &str) -> PanelSide {
        self.sides
            .get(panel)
            .copied()
            .unwrap_or(if matches!(panel, "chat" | "terminal") {
                PanelSide::Right
            } else {
                PanelSide::Left
            })
    }
    pub fn order(&self, panel: &str) -> u8 {
        if let Some(index) = self.order.iter().position(|id| id == panel) {
            return u8::try_from(index.saturating_mul(2)).unwrap_or(255);
        }
        // Insert new History beside Files when restoring an older saved panel order.
        if panel == "history" && !self.order.is_empty() {
            let after = self.order.iter().position(|id| id == "files").unwrap_or(0);
            return u8::try_from(after.saturating_mul(2).saturating_add(1)).unwrap_or(255);
        }
        let rank = match panel {
            "sessions" => 1,
            "files" => 2,
            "history" => 3,
            "search" => 3,
            "git" => 4,
            "editor" => 5,
            "terminal" => 6,
            id if id.starts_with("plugin:") => 7,
            _ => 8,
        };
        match self.sides.get(panel) {
            Some(PanelSide::Left) => rank,
            Some(PanelSide::Right) => 80 + rank,
            None => rank * 10,
        }
    }
    pub fn move_panel(&mut self, panel: &str, right: bool, visible: &[&str]) -> bool {
        let mut panels = vec!["sessions", "files", "history", "editor", "terminal", "chat"];
        panels.extend(self.plugin_panels.keys().map(String::as_str));
        panels.sort_by_key(|id| self.order(id));
        let Some(index) = panels.iter().position(|id| *id == panel) else {
            return false;
        };
        let next = if right {
            ((index + 1)..panels.len()).find(|next| visible.contains(&panels[*next]))
        } else {
            (0..index)
                .rev()
                .find(|next| visible.contains(&panels[*next]))
        };
        let Some(next) = next else {
            return false;
        };
        panels.swap(index, next);
        self.order = panels.into_iter().map(String::from).collect();
        let editor = self.order.iter().position(|id| id == "editor").unwrap_or(2);
        for (index, id) in self.order.iter().enumerate() {
            if id != "editor" {
                self.sides.insert(
                    id.clone(),
                    if index < editor {
                        PanelSide::Left
                    } else {
                        PanelSide::Right
                    },
                );
            }
        }
        true
    }
    pub fn pin(&mut self, panel: &str, side: PanelSide) -> bool {
        if ![
            "sessions", "files", "editor", "chat", "terminal", "git", "search", "history",
        ]
        .contains(&panel)
            && !self.plugin_panels.contains_key(panel)
        {
            return false;
        }
        self.sides.insert(panel.into(), side);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_layout_and_explicit_override_have_the_same_boundary_in_every_workspace() {
        for width in [320.0, 390.0, 799.0] {
            assert!(LayoutMode::Automatic.phone(width));
        }
        for width in [800.0, 1280.0, f64::NAN, f64::INFINITY] {
            assert!(!LayoutMode::Automatic.phone(width));
        }
        assert!(LayoutMode::Phone.phone(1920.0));
        assert!(!LayoutMode::Desktop.phone(320.0));
    }
    #[test]
    fn moving_panels_updates_order_and_resize_edges_and_roundtrips() {
        let mut prefs = LayoutPreferences::default();
        assert!(prefs.move_panel(
            "terminal",
            false,
            &["sessions", "files", "history", "editor", "terminal", "chat"]
        ));
        assert!(prefs.order("terminal") < prefs.order("editor"));
        assert_eq!(prefs.side("terminal"), PanelSide::Left);
        assert!(prefs.move_panel(
            "terminal",
            true,
            &["sessions", "files", "history", "editor", "terminal", "chat"]
        ));
        assert_eq!(prefs.side("terminal"), PanelSide::Right);
        assert!(!prefs.move_panel(
            "sessions",
            false,
            &["sessions", "files", "history", "editor", "terminal", "chat"]
        ));
        let restored: LayoutPreferences =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(restored, prefs);
        assert_eq!(
            serde_json::from_str::<LayoutPreferences>(r#"{"mode":"automatic"}"#)
                .unwrap()
                .files_view,
            FilesView::Explorer
        );
    }
    #[test]
    fn history_joins_older_saved_layouts_beside_files() {
        let prefs: LayoutPreferences =
            serde_json::from_str(r#"{"order":["sessions","files","editor","terminal","chat"]}"#)
                .unwrap();
        assert!(prefs.order("files") < prefs.order("history"));
        assert!(prefs.order("history") < prefs.order("editor"));
    }
    #[test]
    fn pin_preferences_keep_defaults_and_roundtrip_without_viewport_state() {
        let mut preferences = LayoutPreferences::default();
        assert_eq!(preferences.side("chat"), PanelSide::Right);
        assert_eq!(preferences.side("files"), PanelSide::Left);
        assert!(preferences.pin("chat", PanelSide::Left));
        assert!(preferences.pin("terminal", PanelSide::Right));
        assert!(!preferences.pin("unknown", PanelSide::Right));
        preferences.mode = LayoutMode::Phone;
        let encoded = serde_json::to_string(&preferences).unwrap();
        assert_eq!(
            serde_json::from_str::<LayoutPreferences>(&encoded).unwrap(),
            preferences
        );
        assert_eq!(
            serde_json::from_str::<LayoutPreferences>("{}").unwrap(),
            LayoutPreferences::default()
        );
    }
}
