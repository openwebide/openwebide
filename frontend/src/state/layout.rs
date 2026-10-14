use super::responsive::{LAYOUT_PREFERENCES_KEY, LayoutPreferences};
use leptos::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Sessions,
    Files,
    History,
    Editor,
    Chat,
    Terminal,
    Git,
    Search,
}
impl Panel {
    pub const fn requires_project(self) -> bool {
        matches!(
            self,
            Self::Files | Self::History | Self::Editor | Self::Terminal | Self::Git | Self::Search
        )
    }
    pub const fn id(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Files => "files",
            Self::History => "history",
            Self::Editor => "editor",
            Self::Chat => "chat",
            Self::Terminal => "terminal",
            Self::Git => "git",
            Self::Search => "search",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Files => "Files",
            Self::History => "History",
            Self::Editor => "Editor",
            Self::Chat => "Chat",
            Self::Terminal => "Terminal",
            Self::Git => "Git",
            Self::Search => "Search",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PanelVisibility {
    pub sessions: bool,
    pub files: bool,
    pub history: bool,
    pub editor: bool,
    pub chat: bool,
    pub terminal: bool,
    pub git: bool,
    pub search: bool,
}
impl Default for PanelVisibility {
    fn default() -> Self {
        Self {
            sessions: true,
            files: true,
            history: false,
            editor: true,
            chat: true,
            terminal: false,
            git: false,
            search: false,
        }
    }
}
impl PanelVisibility {
    pub const fn for_project(mut self, available: bool) -> Self {
        if !available {
            self.files = false;
            self.history = false;
            self.editor = false;
            self.terminal = false;
            self.git = false;
            self.search = false;
        }
        self
    }
    pub const fn visible(self, panel: Panel) -> bool {
        match panel {
            Panel::Sessions => self.sessions,
            Panel::Files => self.files,
            Panel::History => self.history,
            Panel::Editor => self.editor,
            Panel::Chat => self.chat,
            Panel::Terminal => self.terminal,
            Panel::Git => self.git,
            Panel::Search => self.search,
        }
    }
    pub fn set(&mut self, panel: Panel, visible: bool) {
        match panel {
            Panel::Sessions => self.sessions = visible,
            Panel::Files => self.files = visible,
            Panel::History => self.history = visible,
            Panel::Editor => self.editor = visible,
            Panel::Chat => self.chat = visible,
            Panel::Terminal => self.terminal = visible,
            Panel::Git => self.git = visible,
            Panel::Search => self.search = visible,
        }
    }
}

pub const PANEL_VISIBILITY_KEY: &str = "panel_visibility";
pub const PANEL_RAILS_WIDTH: f64 = 36.0;

/// Fit only open panels, retaining the remembered width of collapsed panels.
pub fn fit_visible_panels(
    viewport: f64,
    widths: [f64; 3],
    visibility: PanelVisibility,
) -> [f64; 3] {
    let fitted = fit_open_panels(
        viewport,
        [
            widths[0],
            widths[1],
            widths[2],
            ActiveResizer::Terminal.default(),
        ],
        visibility,
    );
    [fitted[0], fitted[1], fitted[2]]
}

/// The same sizing policy applies to every resizable tool window.
pub fn fit_open_panels(viewport: f64, widths: [f64; 4], visibility: PanelVisibility) -> [f64; 4] {
    let fitted = fit_history_panels(
        viewport,
        [
            widths[0],
            widths[1],
            widths[2],
            widths[3],
            ActiveResizer::History.default(),
        ],
        visibility,
    );
    [fitted[0], fitted[1], fitted[2], fitted[3]]
}

pub fn fit_history_panels(
    viewport: f64,
    widths: [f64; 5],
    visibility: PanelVisibility,
) -> [f64; 5] {
    let kinds = [
        ActiveResizer::Sidebar,
        ActiveResizer::Tree,
        ActiveResizer::Chat,
        ActiveResizer::Terminal,
        ActiveResizer::History,
    ];
    let visible = [
        visibility.sessions,
        visibility.files || visibility.git || visibility.search,
        visibility.chat,
        false,
        visibility.history,
    ];
    let mut widths = std::array::from_fn(|i| {
        let width = if widths[i].is_finite() {
            widths[i]
        } else {
            kinds[i].default()
        };
        width.clamp(kinds[i].min(), kinds[i].max())
    });
    let center = if visibility.editor { CENTER_MIN } else { 0.0 };
    let used = widths
        .iter()
        .enumerate()
        .filter(|(i, _)| visible[*i])
        .map(|(_, width)| width)
        .sum::<f64>();
    let mut excess = (used + center + PANEL_RAILS_WIDTH - viewport).max(0.0);
    for i in [2, 4, 3, 1, 0] {
        if visible[i] {
            let shrink = excess.min(widths[i] - kinds[i].min());
            widths[i] -= shrink;
            excess -= shrink;
        }
    }
    widths
}

pub const CENTER_MIN: f64 = 260.0;

pub fn fit_panels(viewport: f64, widths: [f64; 3]) -> [f64; 3] {
    fit_visible_panels(
        viewport + PANEL_RAILS_WIDTH,
        widths,
        PanelVisibility::default(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ActiveResizer {
    #[default]
    None,
    Sidebar,
    Tree,
    History,
    Chat,
    Terminal,
}

impl ActiveResizer {
    pub const fn min(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Sidebar => 140.0,
            Self::Tree => 160.0,
            Self::History => 260.0,
            Self::Chat => 260.0,
            Self::Terminal => 140.0,
        }
    }

    pub const fn max(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Sidebar => 480.0,
            Self::Tree => 650.0,
            Self::History => 1200.0,
            Self::Chat => 1000.0,
            Self::Terminal => 700.0,
        }
    }

    pub const fn default(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Sidebar => 240.0,
            Self::Tree => 260.0,
            Self::History => 480.0,
            Self::Chat => 420.0,
            Self::Terminal => 260.0,
        }
    }

    pub const fn setting_key(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Sidebar => "panel_sidebar_width",
            Self::Tree => "panel_tree_width",
            Self::History => "panel_history_width",
            Self::Chat => "panel_chat_width",
            Self::Terminal => "panel_terminal_height",
        }
    }
}

/// User-adjustable widths and shared tool-window layout.
#[derive(Clone, Copy)]
pub struct LayoutState {
    pub panels: RwSignal<PanelVisibility>,
    pub visible_panels: Memo<PanelVisibility>,
    pub active_project: RwSignal<Option<i64>>,
    pub panel_revision: RwSignal<u64>,
    pub width_revision: RwSignal<u64>,
    pub preferences: RwSignal<LayoutPreferences>,
    pub preference_revision: RwSignal<u64>,
    pub viewport_width: RwSignal<f64>,
    pub phone: Memo<bool>,
    pub sheet: RwSignal<Option<Panel>>,
    pub plugin_sheet: RwSignal<Option<String>>,
    pub plugin_panels: RwSignal<std::collections::BTreeSet<String>>,
    pub terminal_cmd: RwSignal<Option<String>>,
    pub sidebar_width: RwSignal<f64>,
    pub tree_width: RwSignal<f64>,
    pub history_width: RwSignal<f64>,
    pub history_tree_width: RwSignal<f64>,
    pub history_tree_revision: RwSignal<u64>,
    pub chat_width: RwSignal<f64>,
    pub terminal_height: RwSignal<f64>,
    pub active_resizer: RwSignal<ActiveResizer>,
}

impl LayoutState {
    pub fn new() -> Self {
        Self::with_active_project(RwSignal::new(None))
    }

    pub fn with_active_project(active_project: RwSignal<Option<i64>>) -> Self {
        let panels = RwSignal::new(PanelVisibility::default());
        let preferences = RwSignal::new(LayoutPreferences::default());
        let viewport_width = RwSignal::new(1200.0);
        let phone =
            Memo::new(move |_| preferences.with(|prefs| prefs.mode.phone(viewport_width.get())));
        let sheet = RwSignal::new(None::<Panel>);
        let plugin_sheet = RwSignal::new(None::<String>);
        let visible_panels = Memo::new(move |_| {
            let mut visible = panels.get().for_project(active_project.get().is_some());
            visible.files |= visible.git || visible.search;
            visible.git = false;
            visible.search = false;
            if phone.get() {
                for panel in [
                    Panel::Sessions,
                    Panel::Files,
                    Panel::History,
                    Panel::Editor,
                    Panel::Chat,
                    Panel::Git,
                    Panel::Search,
                ] {
                    visible.set(
                        panel,
                        plugin_sheet.get().is_none()
                            && sheet.get().unwrap_or(Panel::Chat) == panel
                            && (!panel.requires_project() || active_project.get().is_some()),
                    );
                }
            }
            visible
        });
        Self {
            panels,
            visible_panels,
            active_project,
            panel_revision: RwSignal::new(0),
            width_revision: RwSignal::new(0),
            preferences,
            preference_revision: RwSignal::new(0),
            viewport_width,
            phone,
            sheet,
            plugin_sheet,
            plugin_panels: RwSignal::new(Default::default()),
            terminal_cmd: RwSignal::new(None),
            sidebar_width: RwSignal::new(ActiveResizer::Sidebar.default()),
            tree_width: RwSignal::new(ActiveResizer::Tree.default()),
            history_width: RwSignal::new(ActiveResizer::History.default()),
            history_tree_width: RwSignal::new(260.0),
            history_tree_revision: RwSignal::new(0),
            chat_width: RwSignal::new(ActiveResizer::Chat.default()),
            terminal_height: RwSignal::new(ActiveResizer::Terminal.default()),
            active_resizer: RwSignal::new(ActiveResizer::None),
        }
    }

    pub fn flexible_panel(&self) -> Option<Panel> {
        let visible = self.visible_panels.get();
        let rightmost = self.preferences.with(|prefs| {
            let has_plugin_after_history = prefs.plugin_panels.iter().any(|(id, panel)| {
                panel.visible
                    && self
                        .plugin_panels
                        .with(|registered| registered.contains(id))
                    && prefs.order(id) > prefs.order(Panel::History.id())
            });
            if has_plugin_after_history {
                return None;
            }
            [
                Panel::Sessions,
                Panel::Files,
                Panel::History,
                Panel::Editor,
                Panel::Chat,
            ]
            .into_iter()
            .filter(|panel| visible.visible(*panel))
            .max_by_key(|panel| prefs.order(panel.id()))
        });
        if rightmost == Some(Panel::History) {
            Some(Panel::History)
        } else if visible.editor {
            Some(Panel::Editor)
        } else if visible.chat {
            Some(Panel::Chat)
        } else {
            None
        }
    }

    pub fn width(&self, kind: ActiveResizer) -> RwSignal<f64> {
        match kind {
            ActiveResizer::Sidebar => self.sidebar_width,
            ActiveResizer::Tree => self.tree_width,
            ActiveResizer::History => self.history_width,
            ActiveResizer::Chat => self.chat_width,
            ActiveResizer::Terminal => self.terminal_height,
            ActiveResizer::None => panic!("a panel width requires a resize target"),
        }
    }
    pub fn available(&self, panel: Panel) -> bool {
        !panel.requires_project() || self.active_project.get().is_some()
    }

    pub fn fit(&self, viewport: f64) {
        self.viewport_width.set(viewport);
        if self.phone.get_untracked() {
            return;
        }
        let kinds = [
            ActiveResizer::Sidebar,
            ActiveResizer::Tree,
            ActiveResizer::Chat,
            ActiveResizer::Terminal,
            ActiveResizer::History,
        ];
        let mut widths = kinds.map(|kind| self.width(kind).get_untracked());
        let flexible_history = untrack(|| self.flexible_panel() == Some(Panel::History));
        if flexible_history {
            widths[4] = ActiveResizer::History.min();
        }
        let plugin_width = self.preferences.with_untracked(|prefs| {
            prefs
                .plugin_panels
                .iter()
                .filter(|(namespace, panel)| {
                    panel.visible
                        && self
                            .plugin_panels
                            .with_untracked(|registered| registered.contains(*namespace))
                })
                .count()
        }) as f64
            * 360.0;
        for (kind, width) in kinds.into_iter().zip(fit_history_panels(
            (viewport - plugin_width).max(0.0),
            widths,
            self.visible_panels.get_untracked(),
        )) {
            if kind != ActiveResizer::History || !flexible_history {
                self.width(kind).set(width);
            }
        }
    }

    pub fn restore_preferences(&self, values: &std::collections::BTreeMap<String, String>) {
        if self.preference_revision.get_untracked() == 0 {
            self.preferences.set(
                values
                    .get(LAYOUT_PREFERENCES_KEY)
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_default(),
            );
        }
    }

    pub fn restore_panels(&self, value: Option<&String>) {
        if self.panel_revision.get_untracked() == 0 {
            let mut panels: PanelVisibility = value
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default();
            if panels.git || panels.search {
                panels.files = true;
                if self.preference_revision.get_untracked() == 0 {
                    self.preferences.update(|prefs| {
                        if prefs.files_view == super::responsive::FilesView::Explorer {
                            prefs.files_view = if panels.git {
                                super::responsive::FilesView::Changes
                            } else {
                                super::responsive::FilesView::Search
                            };
                        }
                    });
                }
                panels.git = false;
                panels.search = false;
            }
            self.panels.set(panels);
        }
    }

    pub fn clamp_visible(&self, resizer: ActiveResizer, requested: f64, viewport: f64) -> f64 {
        let panels = self.visible_panels.get_untracked();
        let sidebar = if panels.sessions {
            self.sidebar_width.get_untracked()
        } else {
            0.0
        };
        let tree = if panels.files {
            self.tree_width.get_untracked()
        } else {
            0.0
        };
        let chat = if panels.chat {
            self.chat_width.get_untracked()
        } else {
            0.0
        };
        let history = if panels.history {
            if self.flexible_panel() == Some(Panel::History) {
                ActiveResizer::History.min()
            } else {
                self.history_width.get_untracked()
            }
        } else {
            0.0
        };
        if resizer == ActiveResizer::History {
            return requested.clamp(resizer.min(), resizer.max()).min(
                (viewport
                    - PANEL_RAILS_WIDTH
                    - sidebar
                    - tree
                    - chat
                    - if panels.editor { CENTER_MIN } else { 0.0 })
                .max(resizer.min()),
            );
        }
        let viewport =
            viewport - PANEL_RAILS_WIDTH - history + if panels.editor { 0.0 } else { CENTER_MIN };
        Self::clamp(resizer, requested, sidebar, tree, chat, viewport)
    }

    /// Clamp a requested panel width to its bounds while keeping room for the
    /// center editor, if the existing outer panels leave enough room.
    pub fn clamp(
        resizer: ActiveResizer,
        requested: f64,
        sidebar_width: f64,
        tree_width: f64,
        chat_width: f64,
        total_width: f64,
    ) -> f64 {
        if resizer == ActiveResizer::None {
            return requested;
        }

        let other_width = match resizer {
            ActiveResizer::None => 0.0,
            ActiveResizer::Sidebar => tree_width + chat_width,
            ActiveResizer::Tree => sidebar_width + chat_width,
            ActiveResizer::Chat => sidebar_width + tree_width,
            ActiveResizer::Terminal | ActiveResizer::History => {
                sidebar_width + tree_width + chat_width
            }
        };
        let lower = resizer.min();
        let upper = resizer.max();
        let width = requested.clamp(lower, upper);
        let max_for_center = (total_width - other_width - CENTER_MIN).max(lower);

        width.min(max_for_center)
    }
}

impl Default for LayoutState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_widths(actual: [f64; 3], expected: [f64; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn late_layout_restore_keeps_current_preferences_and_open_tools_keep_editor_space() {
        Owner::new().with(|| {
            let layout = LayoutState::new();
            let mut values = std::collections::BTreeMap::new();
            values.insert(
                LAYOUT_PREFERENCES_KEY.into(),
                r#"{"mode":"desktop","sides":{"chat":"left"}}"#.into(),
            );
            layout.restore_preferences(&values);
            assert_eq!(
                layout.preferences.get_untracked().side("chat"),
                super::super::responsive::PanelSide::Left
            );
            layout
                .preferences
                .update(|prefs| prefs.mode = super::super::responsive::LayoutMode::Phone);
            layout.preference_revision.set(1);
            layout.restore_preferences(&values);
            assert_eq!(
                layout.preferences.get_untracked().mode,
                super::super::responsive::LayoutMode::Phone
            );
        });
        let mut visibility = PanelVisibility {
            git: true,
            ..PanelVisibility::default()
        };
        assert_widths(
            fit_visible_panels(1400.0, [240.0, 260.0, 420.0], visibility),
            [240.0, 260.0, 420.0],
        );
        visibility.terminal = true;
        assert_widths(
            fit_visible_panels(1400.0, [240.0, 260.0, 420.0], visibility),
            [240.0, 260.0, 420.0],
        );
    }

    #[test]
    fn history_width_releases_space_when_collapsed_and_preserves_editor_space() {
        let visible = PanelVisibility {
            history: true,
            ..Default::default()
        };
        let widths = fit_history_panels(1600.0, [240.0, 260.0, 420.0, 260.0, 480.0], visible);
        assert!(
            widths[0] + widths[1] + widths[2] + widths[4] + CENTER_MIN + PANEL_RAILS_WIDTH
                <= 1600.0
        );
        assert!(widths[4] >= ActiveResizer::History.min());
        let hidden = PanelVisibility {
            history: false,
            ..visible
        };
        assert!(
            (fit_history_panels(1200.0, [240.0, 260.0, 420.0, 260.0, 700.0], hidden)[4] - 700.0)
                .abs()
                < f64::EPSILON
        );
        let restored: PanelVisibility = serde_json::from_str(r#"{"files":true}"#).unwrap();
        assert!(!restored.history);
        assert!(!visible.for_project(false).history);
    }

    #[test]
    fn terminal_height_does_not_consume_horizontal_space() {
        let visible = PanelVisibility {
            terminal: true,
            ..Default::default()
        };
        let widths = fit_open_panels(1400.0, [240.0, 260.0, 420.0, 500.0], visible);
        assert_widths([widths[0], widths[1], widths[2]], [240.0, 260.0, 420.0]);
        assert!((widths[3] - 500.0).abs() < f64::EPSILON);
        let hidden = PanelVisibility {
            terminal: false,
            ..visible
        };
        assert!(
            (fit_open_panels(1000.0, [240.0, 260.0, 420.0, 500.0], hidden)[3] - 500.0).abs()
                < f64::EPSILON
        );
        assert!(
            (fit_open_panels(2400.0, [240.0, 260.0, 420.0, f64::NAN], visible)[3]
                - ActiveResizer::Terminal.default())
            .abs()
                < f64::EPSILON
        );
        Owner::new().with(|| {
            let layout = LayoutState::new();
            let encoded = r#"{"files":false,"git":true}"#.to_string();
            layout.restore_panels(Some(&encoded));
            let restored = layout.panels.get_untracked();
            assert!(restored.files && !restored.git && !restored.search);
            assert_eq!(
                layout.preferences.get_untracked().files_view,
                super::super::responsive::FilesView::Changes
            );
            layout.panels.update(|panels| panels.files = false);
            assert!(!layout.panels.get_untracked().files);
        });
    }

    #[test]
    fn flexible_panel_expands_history_when_no_subsequent_panel_visible() {
        Owner::new().with(|| {
            let layout = LayoutState::new();
            layout.active_project.set(Some(1));

            // Default: editor is visible -> editor is flexible
            assert_eq!(layout.flexible_panel(), Some(Panel::Editor));

            // History open, editor closed, chat open -> chat is flexible
            layout.panels.set(PanelVisibility {
                sessions: true,
                files: true,
                history: true,
                editor: false,
                chat: true,
                ..Default::default()
            });
            assert_eq!(layout.flexible_panel(), Some(Panel::Chat));

            // History open, editor closed, chat closed -> history expands to fill remaining width
            layout.panels.update(|panels| {
                panels.chat = false;
            });
            assert_eq!(layout.flexible_panel(), Some(Panel::History));
        });
    }

    #[test]
    fn collapsed_panels_keep_their_width_and_release_center_space() {
        let hidden = PanelVisibility {
            sessions: false,
            files: false,
            history: false,
            editor: true,
            chat: true,
            terminal: false,
            git: false,
            search: false,
        };
        assert_widths(
            fit_visible_panels(900.0, [480.0, 650.0, 800.0], hidden),
            [480.0, 650.0, 604.0],
        );
        let chat_only = PanelVisibility {
            editor: false,
            ..hidden
        };
        assert_widths(
            fit_visible_panels(900.0, [480.0, 650.0, 800.0], chat_only),
            [480.0, 650.0, 800.0],
        );
        assert_widths(
            fit_visible_panels(100.0, [480.0, 650.0, 800.0], hidden),
            [480.0, 650.0, 260.0],
        );
    }

    #[test]
    fn fit_shrinks_chat_then_tree_then_sidebar() {
        let fitted = fit_panels(1280.0, [480.0, 650.0, 1000.0]);
        assert_widths(fitted, [480.0, 280.0, 260.0]);
        assert!(fitted.iter().sum::<f64>() <= 1020.0);
        assert_widths(
            fit_panels(900.0, [240.0, 260.0, 420.0]),
            [220.0, 160.0, 260.0],
        );
    }

    #[test]
    fn wide_viewports_preserve_widths_and_tiny_viewports_use_minimums() {
        assert_widths(
            fit_panels(3000.0, [480.0, 650.0, 1000.0]),
            [480.0, 650.0, 1000.0],
        );
        assert_widths(
            fit_panels(100.0, [480.0, 650.0, 1000.0]),
            [140.0, 160.0, 260.0],
        );
        assert_widths(
            fit_panels(3000.0, [1.0, 900.0, 2000.0]),
            [140.0, 650.0, 1000.0],
        );
    }

    #[test]
    fn resizer_bounds_defaults_and_settings_match_the_panel_table() {
        let cases = [
            (
                ActiveResizer::Sidebar,
                140.0,
                480.0,
                240.0,
                "panel_sidebar_width",
            ),
            (ActiveResizer::Tree, 160.0, 650.0, 260.0, "panel_tree_width"),
            (
                ActiveResizer::Chat,
                260.0,
                1000.0,
                420.0,
                "panel_chat_width",
            ),
        ];

        for (resizer, min, max, default, setting_key) in cases {
            assert!((resizer.min() - min).abs() < f64::EPSILON);
            assert!((resizer.max() - max).abs() < f64::EPSILON);
            assert!((resizer.default() - default).abs() < f64::EPSILON);
            assert_eq!(resizer.setting_key(), setting_key);
        }
    }

    #[test]
    fn clamp_respects_panel_bounds_and_center_minimum() {
        let cases = [
            (
                ActiveResizer::Sidebar,
                10.0,
                240.0,
                260.0,
                420.0,
                1200.0,
                140.0,
            ),
            (
                ActiveResizer::Tree,
                900.0,
                240.0,
                260.0,
                420.0,
                5000.0,
                650.0,
            ),
            (
                ActiveResizer::Chat,
                1100.0,
                240.0,
                260.0,
                420.0,
                5000.0,
                1000.0,
            ),
            (
                ActiveResizer::Chat,
                600.0,
                240.0,
                260.0,
                420.0,
                1200.0,
                440.0,
            ),
        ];

        for (resizer, requested, sidebar, tree, chat, total, expected) in cases {
            assert!(
                (LayoutState::clamp(resizer, requested, sidebar, tree, chat, total) - expected)
                    .abs()
                    < f64::EPSILON
            );
        }
    }
}

pub fn history_tree_width(container: f64, requested: f64) -> Option<f64> {
    if !container.is_finite() || container < 426.0 {
        return None;
    }
    Some(
        if requested.is_finite() {
            requested
        } else {
            260.0
        }
        .clamp(160.0, (container - 266.0).min(650.0)),
    )
}
#[cfg(test)]
mod history_tree_tests {
    use super::*;
    #[test]
    fn internal_tree_keeps_viewer_reserve_and_stacks_when_too_small() {
        assert_eq!(history_tree_width(425.0, 260.0), None);
        assert_eq!(history_tree_width(426.0, 650.0), Some(160.0));
        assert_eq!(history_tree_width(900.0, 650.0), Some(634.0));
        assert_eq!(history_tree_width(1200.0, 900.0), Some(650.0));
        assert_eq!(history_tree_width(1200.0, f64::NAN), Some(260.0));
    }
}
