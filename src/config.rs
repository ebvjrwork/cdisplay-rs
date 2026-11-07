//! Settings. CDisplay kept these under HKCU\Software\CDisplay; here they live in
//! a TOML file (`~/.config/cdisplay-rs/config.toml` on Linux).

use crate::filters::{EdgeYellowParams, YellowParams};
use crate::layout::SizeMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Background {
    /// Colour sampled from the page edges.
    Auto,
    Black,
    White,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WheelAction {
    /// Scroll the page; turn the page at the top/bottom.
    ScrollPage,
    /// Turn pages directly.
    TurnPage,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClickAction {
    DragImage,
    NextPage,
    PreviousPage,
    /// Left half of the window goes back, right half goes forward (mirrored in Japanese mode).
    NextOrPrevious,
    None,
}

impl ClickAction {
    pub const ALL: [ClickAction; 5] = [
        ClickAction::DragImage,
        ClickAction::NextPage,
        ClickAction::PreviousPage,
        ClickAction::NextOrPrevious,
        ClickAction::None,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ClickAction::DragImage => "Drag Image",
            ClickAction::NextPage => "Next Page",
            ClickAction::PreviousPage => "Previous Page",
            ClickAction::NextOrPrevious => "Next or Previous",
            ClickAction::None => "(None)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // Image sizing
    pub size_mode: SizeMode,
    pub target_height: u32,
    pub two_pages: bool,
    pub suppress_fit_width_for_double: bool,
    pub rotate_scroll_functions: bool,

    // Colour
    pub colour_balance: bool,
    pub discards_per_1000: i32,
    pub gamma_correction: bool,
    pub gamma_x100: i32,
    pub yellow_reduction: bool,
    pub yellow: YellowParams,
    pub edge_yellow_reduction: bool,
    pub internal_yellow_reduction: bool,
    pub edge_yellow: EdgeYellowParams,
    /// Smooth (mip-mapped) scaling. The original had none; on by default.
    pub smooth_scaling: bool,

    // Program settings
    pub background: Background,
    pub image_information: bool,
    pub zooming: bool,
    pub zoom_factor: f32,
    pub japanese_mode: bool,
    pub small_cursor: bool,
    pub page_in_twos: bool,
    pub page_back_in_twos: bool,
    pub display_info_files: bool,
    pub page_buffer: usize,
    pub slideshow_seconds: f32,
    pub use_system_file_dialog: bool,
    pub start_fullscreen: bool,

    // Mouse
    pub left_click: ClickAction,
    pub right_double_click_next: bool,
    pub wheel: WheelAction,

    // State
    pub last_dir: Option<PathBuf>,
    pub recent: Vec<RecentItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentItem {
    pub path: PathBuf,
    pub page: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            size_mode: SizeMode::FitWidthIfOversize,
            target_height: 2048,
            two_pages: false,
            suppress_fit_width_for_double: true,
            rotate_scroll_functions: true,
            colour_balance: false,
            discards_per_1000: 3,
            gamma_correction: false,
            gamma_x100: 100,
            yellow_reduction: false,
            yellow: YellowParams::default(),
            edge_yellow_reduction: false,
            internal_yellow_reduction: false,
            edge_yellow: EdgeYellowParams::default(),
            smooth_scaling: true,
            background: Background::Auto,
            image_information: false,
            zooming: false,
            zoom_factor: 2.0,
            japanese_mode: false,
            small_cursor: false,
            page_in_twos: true,
            page_back_in_twos: true,
            display_info_files: true,
            page_buffer: 3,
            slideshow_seconds: 5.0,
            use_system_file_dialog: false,
            start_fullscreen: true,
            left_click: ClickAction::DragImage,
            right_double_click_next: true,
            wheel: WheelAction::ScrollPage,
            last_dir: None,
            recent: Vec::new(),
        }
    }
}

pub const MAX_RECENT: usize = 20;

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "cdisplay-rs")
            .map(|d| d.config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable config {}: {e}", p.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match toml::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = std::fs::write(&p, s) {
                    log::warn!("saving config: {e}");
                }
            }
            Err(e) => log::warn!("serialising config: {e}"),
        }
    }

    /// Settings that change decoded pixels; pages are reprocessed when these change.
    pub fn processing_key(&self) -> ProcessingKey {
        ProcessingKey {
            balance: self.colour_balance.then_some(self.discards_per_1000),
            gamma: self.gamma_correction.then_some(self.gamma_x100),
            yellow: self.yellow_reduction.then_some(self.yellow),
            edge: self.edge_yellow_reduction.then_some(self.edge_yellow),
            internal: self.internal_yellow_reduction.then_some(self.edge_yellow),
        }
    }

    pub fn remember(&mut self, path: PathBuf, page: usize) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, RecentItem { path, page });
        self.recent.truncate(MAX_RECENT);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessingKey {
    pub balance: Option<i32>,
    pub gamma: Option<i32>,
    pub yellow: Option<YellowParams>,
    pub edge: Option<EdgeYellowParams>,
    pub internal: Option<EdgeYellowParams>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toml_roundtrip_and_partial() {
        let mut c = Config::default();
        c.remember("/tmp/a.cbz".into(), 4);
        let s = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<Config>(&s).unwrap(), c);
        let partial: Config = toml::from_str("two_pages = true").unwrap();
        assert!(partial.two_pages);
        assert_eq!(partial.target_height, 2048);
    }
}
