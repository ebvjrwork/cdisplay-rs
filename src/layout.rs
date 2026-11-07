//! Page sizing, rotation and two-page spread logic (pure functions, no GUI).

use serde::{Deserialize, Serialize};

/// CDisplay's "Image Size Options". The original used a group of exclusive
/// checkboxes; here they are one enum. Shortcuts are the originals (C = Ctrl, S = Shift).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeMode {
    /// No scaling.
    Actual,
    /// C+F
    Fit,
    /// C+S+F
    FitIfOversize,
    /// C+W
    FitWidth,
    /// C+S+W
    FitWidthIfOversize,
    /// C+H
    FitHeight,
    /// C+S+H
    FitHeightIfOversize,
    /// C+S — each page drawn at `target_height` pixels.
    TargetHeight,
}

impl SizeMode {
    pub const ALL: [SizeMode; 8] = [
        SizeMode::Actual,
        SizeMode::Fit,
        SizeMode::FitIfOversize,
        SizeMode::FitWidth,
        SizeMode::FitWidthIfOversize,
        SizeMode::FitHeight,
        SizeMode::FitHeightIfOversize,
        SizeMode::TargetHeight,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SizeMode::Actual => "Actual Size",
            SizeMode::Fit => "Fit (Ctrl+F)",
            SizeMode::FitIfOversize => "Fit If Oversize (Ctrl+Shift+F)",
            SizeMode::FitWidth => "Fit Width (Ctrl+W)",
            SizeMode::FitWidthIfOversize => "Fit Width If Oversize (Ctrl+Shift+W)",
            SizeMode::FitHeight => "Fit Height (Ctrl+H)",
            SizeMode::FitHeightIfOversize => "Fit Height If Oversize (Ctrl+Shift+H)",
            SizeMode::TargetHeight => "Set Height (Ctrl+S)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    Normal,
    Left,
    Right,
    Invert,
}

impl Rotation {
    fn quarter_turns_cw(self) -> u8 {
        match self {
            Rotation::Normal => 0,
            Rotation::Right => 1,
            Rotation::Invert => 2,
            Rotation::Left => 3,
        }
    }
    fn from_quarter_turns(n: u8) -> Self {
        match n % 4 {
            0 => Rotation::Normal,
            1 => Rotation::Right,
            2 => Rotation::Invert,
            _ => Rotation::Left,
        }
    }
    /// Combine the "all images" rotation with a per-page rotation.
    pub fn compose(self, other: Rotation) -> Rotation {
        Self::from_quarter_turns(self.quarter_turns_cw() + other.quarter_turns_cw())
    }
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Left | Rotation::Right)
    }
    pub fn apply(self, img: &image::RgbaImage) -> image::RgbaImage {
        match self {
            Rotation::Normal => img.clone(),
            Rotation::Right => image::imageops::rotate90(img),
            Rotation::Invert => image::imageops::rotate180(img),
            Rotation::Left => image::imageops::rotate270(img),
        }
    }
}

/// Scale factor for content of `content` size (w, h) in a viewport.
pub fn scale_for(
    mode: SizeMode,
    content: (f32, f32),
    viewport: (f32, f32),
    target_height: u32,
) -> f32 {
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let (vw, vh) = viewport;
    let fit_w = vw / cw;
    let fit_h = vh / ch;
    match mode {
        SizeMode::Actual => 1.0,
        SizeMode::Fit => fit_w.min(fit_h),
        SizeMode::FitIfOversize => fit_w.min(fit_h).min(1.0),
        SizeMode::FitWidth => fit_w,
        SizeMode::FitWidthIfOversize => fit_w.min(1.0),
        SizeMode::FitHeight => fit_h,
        SizeMode::FitHeightIfOversize => fit_h.min(1.0),
        SizeMode::TargetHeight => target_height.max(1) as f32 / ch,
    }
}

/// A spread is a single page, or two pages shown side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub first: usize,
    pub second: Option<usize>,
}

impl Spread {
    pub fn len(&self) -> usize {
        1 + self.second.is_some() as usize
    }
    /// Pages in on-screen left-to-right order.
    pub fn screen_order(&self, japanese: bool) -> Vec<usize> {
        match (self.second, japanese) {
            (None, _) => vec![self.first],
            (Some(s), false) => vec![self.first, s],
            (Some(s), true) => vec![s, self.first],
        }
    }
}

/// Decide what to show starting at `page`. `is_wide(i)` reports whether page `i`
/// is a double-width (landscape) page — those are always shown alone, as are
/// the last page and pages whose size is still unknown.
pub fn spread_at(
    page: usize,
    count: usize,
    two_pages: bool,
    is_wide: impl Fn(usize) -> Option<bool>,
) -> Spread {
    if !two_pages || page + 1 >= count {
        return Spread {
            first: page,
            second: None,
        };
    }
    match (is_wide(page), is_wide(page + 1)) {
        (Some(false), Some(false)) => Spread {
            first: page,
            second: Some(page + 1),
        },
        _ => Spread {
            first: page,
            second: None,
        },
    }
}

/// Page to move to on "Next Page".
pub fn next_page(current: Spread, count: usize, page_in_twos: bool) -> Option<usize> {
    let step = if page_in_twos { current.len() } else { 1 };
    let n = current.first + step;
    (n < count).then_some(n)
}

/// Page to move to on "Previous Page". In two-page mode with "Backward Two
/// Pages" this steps back two when the previous two pages would form a spread.
pub fn prev_page(
    current: Spread,
    two_pages: bool,
    page_back_in_twos: bool,
    is_wide: impl Fn(usize) -> Option<bool>,
) -> Option<usize> {
    if current.first == 0 {
        return None;
    }
    if two_pages && page_back_in_twos && current.first >= 2 {
        let cand = current.first - 2;
        if matches!(
            (is_wide(cand), is_wide(cand + 1)),
            (Some(false), Some(false))
        ) {
            return Some(cand);
        }
    }
    Some(current.first - 1)
}

/// What one press of Space does next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpaceStep {
    /// Scroll to this (x, y) offset.
    Scroll(f32, f32),
    NextPage,
}

/// Space (and each automatic-slideshow tick): scroll down a screen; at the
/// bottom move one screen across (right, or left in Japanese mode) and back to
/// the top; once the whole page has been seen, turn the page. `scroll`, `view`
/// and `max` are (x, y) in screen points.
pub fn space_step(
    scroll: (f32, f32),
    view: (f32, f32),
    max: (f32, f32),
    japanese: bool,
) -> SpaceStep {
    let (x, y) = scroll;
    if y < max.1 - 0.5 {
        return SpaceStep::Scroll(x, y + view.1 * 0.9);
    }
    let more_across = if japanese { x > 0.5 } else { x < max.0 - 0.5 };
    if more_across {
        let dx = view.0 * 0.9;
        return SpaceStep::Scroll(if japanese { x - dx } else { x + dx }, 0.0);
    }
    SpaceStep::NextPage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales() {
        let c = (1000.0, 2000.0);
        let v = (1920.0, 1080.0);
        assert_eq!(scale_for(SizeMode::FitHeight, c, v, 0), 0.54);
        assert_eq!(scale_for(SizeMode::FitWidth, c, v, 0), 1.92);
        assert_eq!(scale_for(SizeMode::FitWidthIfOversize, c, v, 0), 1.0);
        assert_eq!(scale_for(SizeMode::Fit, c, v, 0), 0.54);
        assert_eq!(scale_for(SizeMode::TargetHeight, c, v, 1000), 0.5);
    }

    #[test]
    fn rotation_compose() {
        assert_eq!(Rotation::Left.compose(Rotation::Right), Rotation::Normal);
        assert_eq!(Rotation::Right.compose(Rotation::Right), Rotation::Invert);
        assert!(Rotation::Left.swaps_axes());
    }

    #[test]
    fn spreads() {
        let wide = |i: usize| Some(i == 3);
        assert_eq!(
            spread_at(0, 6, true, wide),
            Spread {
                first: 0,
                second: Some(1)
            }
        );
        assert_eq!(
            spread_at(2, 6, true, wide),
            Spread {
                first: 2,
                second: None
            }
        );
        assert_eq!(
            spread_at(3, 6, true, wide),
            Spread {
                first: 3,
                second: None
            }
        );
        assert_eq!(
            spread_at(5, 6, true, wide),
            Spread {
                first: 5,
                second: None
            }
        );
        let s = spread_at(0, 6, true, wide);
        assert_eq!(next_page(s, 6, true), Some(2));
        assert_eq!(next_page(s, 6, false), Some(1));
        assert_eq!(s.screen_order(true), vec![1, 0]);
        assert_eq!(
            prev_page(
                Spread {
                    first: 2,
                    second: None
                },
                true,
                true,
                wide
            ),
            Some(0)
        );
        assert_eq!(
            prev_page(
                Spread {
                    first: 5,
                    second: None
                },
                true,
                true,
                wide
            ),
            Some(4)
        );
        assert_eq!(
            prev_page(
                Spread {
                    first: 0,
                    second: None
                },
                true,
                true,
                wide
            ),
            None
        );
    }

    #[test]
    fn space_reads_down_then_across_then_turns_page() {
        let view = (100.0, 100.0);
        // Page fits the screen: straight to the next page.
        assert_eq!(space_step((0.0, 0.0), view, (0.0, 0.0), false), SpaceStep::NextPage);
        // Tall page: down a screen (90%), then next page once at the bottom.
        let tall = (0.0, 150.0);
        assert_eq!(space_step((0.0, 0.0), view, tall, false), SpaceStep::Scroll(0.0, 90.0));
        assert_eq!(space_step((0.0, 150.0), view, tall, false), SpaceStep::NextPage);
        // Wide and tall: at the bottom, move one screen right and back to the top.
        let big = (150.0, 150.0);
        assert_eq!(space_step((0.0, 150.0), view, big, false), SpaceStep::Scroll(90.0, 0.0));
        assert_eq!(space_step((150.0, 150.0), view, big, false), SpaceStep::NextPage);
        // Japanese mode reads right to left: move left, finish at x = 0.
        assert_eq!(space_step((150.0, 150.0), view, big, true), SpaceStep::Scroll(60.0, 0.0));
        assert_eq!(space_step((0.0, 150.0), view, big, true), SpaceStep::NextPage);
    }
}
