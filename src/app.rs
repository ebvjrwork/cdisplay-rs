//! The viewer window: page display, keyboard/mouse handling, menus and dialogs.
//! Shortcut keys follow CDisplay 1.8 (see `HELP_TEXT`).

use crate::config::{Background, ClickAction, Config, ProcessingKey, WheelAction};
use crate::filters::BalanceOutcome;
use crate::layout::{self, Rotation, SizeMode, Spread};
use crate::loader::{Job, JobKind, Loader};
use crate::source::{self, Book};
use eframe::egui::{
    self, Align2, Color32, ColorImage, Context, CursorIcon, Event, FontId, Key, Modifiers,
    PointerButton, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2, ViewportCommand, pos2,
    vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct StartRequest {
    pub path: Option<PathBuf>,
    pub page: Option<usize>,
    pub resume: bool,
}

const HELP_TEXT: &str = "\
Reading
  Page Down / Right-double-click   Next page
  Page Up                          Previous page
  Space / Shift+Space              Scroll down, then next page (and back)
  Arrow keys                       Scroll; Left/Right turn pages when nothing to scroll
  Home / End                       First / last page
  P / Shift+P                      Go to page: thumbnails / slider
  A                                Automatic slideshow

Files
  L                                Load files
  Shift+L / Shift+K                Load next / previous file in the folder
  R                                Resume reading where you left off
  Drag & drop                      Open a file or folder

Display (Ctrl = C, Shift = S, as in the original's configure dialog)
  Ctrl+F / Ctrl+Shift+F            Fit / Fit if oversize
  Ctrl+W / Ctrl+Shift+W            Fit width / Fit width if oversize
  Ctrl+H / Ctrl+Shift+H            Fit height / Fit height if oversize
  Ctrl+S                           Set height (configured pixel height)
  Ctrl+T                           Two pages side by side
  Ctrl+J                           Japanese (right-to-left) mode
  Ctrl+L / Ctrl+R                  Rotate all pages left / right
  Ctrl+B / Ctrl+G / Ctrl+Y         Colour balance / gamma / yellow reduction
  Ctrl+I                           Image information
  Ctrl+Z                           Magnifying glass
  Ctrl+C                           Small cursor

Program
  C                                Configure
  F10 / right-click                Menu
  W                                Windowed / full screen
  M                                Minimise
  F1                               This help
  X / Esc                          Exit (your place is saved)";

#[derive(Debug, Clone, PartialEq)]
enum Cmd {
    Open,
    OpenPath(PathBuf, Option<usize>),
    NextFile,
    PrevFile,
    Resume,
    Exit,
    Minimize,
    ToggleWindowed,
    Next,
    Prev,
    First,
    Last,
    Goto(usize),
    Thumbnails,
    Slider,
    Slideshow,
    Configure,
    RotatePage(Rotation),
    RotateAll(Rotation),
    ToggleRotateAll(Rotation),
    SaveToFile,
    CopyToClipboard,
    Help,
    About,
    ShowInfoFiles,
    SetSize(SizeMode),
    ToggleTwoPages,
    ToggleJapanese,
    ToggleBalance,
    ToggleGamma,
    ToggleYellow,
    ToggleInfo,
    ToggleZoom,
    ToggleSmallCursor,
    ToggleRotateScroll,
    ToggleMenuBar,
}

struct PageInfo {
    original_size: (u32, u32),
    format: String,
    file_bytes: usize,
    balance: Option<BalanceOutcome>,
    yellow_applied: Option<bool>,
    edge_colour: [u8; 3],
    millis: u128,
}

struct PageTex {
    tex: TextureHandle,
    size: Vec2,
    rotation: Rotation,
    processing: ProcessingKey,
    info: PageInfo,
}

/// What the reader draws for one page this frame (copied out of the cache so
/// input handling can mutate the app while drawing).
#[derive(Clone, Copy)]
struct Shown {
    index: usize,
    tex: egui::TextureId,
    size: Vec2,
    edge: [u8; 3],
}

struct Thumb {
    tex: TextureHandle,
    rotation: Rotation,
}

enum Dialog {
    None,
    Config(Box<Config>),
    Slider { value: usize, text: String },
    Thumbnails { scroll_to_current: bool },
    Help,
    About,
    Info { title: String, text: String },
    Browser(Browser),
    Message(String),
}

/// CDisplay shipped its own "customised file open" dialog; this is the
/// equivalent, with the desktop's dialog available as an option.
struct Browser {
    dir: PathBuf,
    entries: Vec<(String, PathBuf, bool)>,
    filter: String,
}

impl Browser {
    fn new(dir: PathBuf) -> Self {
        let mut b = Browser {
            dir,
            entries: Vec::new(),
            filter: String::new(),
        };
        b.refresh();
        b
    }
    fn refresh(&mut self) {
        let mut v: Vec<(String, PathBuf, bool)> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') {
                            return None;
                        }
                        let p = e.path();
                        let is_dir = p.is_dir();
                        (is_dir || source::is_archive_name(&name) || source::is_image_name(&name))
                            .then_some((name, p, is_dir))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| source::natural_cmp(&a.0, &b.0)));
        self.entries = v;
    }
}

/// A book being opened on a background thread: (path, result, requested page).
type Opening = (
    PathBuf,
    crossbeam_channel::Receiver<anyhow::Result<(Book, usize)>>,
    Option<usize>,
);

pub struct App {
    cfg: Config,
    book: Option<Arc<Book>>,
    loader: Option<Loader>,
    page: usize,
    pages: HashMap<usize, PageTex>,
    errors: HashMap<usize, String>,
    thumbs: HashMap<usize, Thumb>,
    queued: Vec<Job>,
    rot_all: Rotation,
    rot_page: HashMap<usize, Rotation>,
    scroll: Vec2,
    scroll_to_end: Option<bool>,
    wheel_overscroll: f32,
    opening: Option<Opening>,
    /// Info-file text being read, and the page the reader was on when it was requested.
    info_rx: Option<(crossbeam_channel::Receiver<(String, String)>, usize)>,
    dialog: Dialog,
    show_menu: bool,
    slideshow: Option<Instant>,
    toast: Option<(String, Instant)>,
    max_side: u32,
    title: String,
    last_view: Rect,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, start: StartRequest) -> Self {
        let max_side = cc.egui_ctx.input(|i| i.max_texture_side).clamp(2048, 16384) as u32;
        let mut app = App {
            cfg,
            book: None,
            loader: None,
            page: 0,
            pages: HashMap::new(),
            errors: HashMap::new(),
            thumbs: HashMap::new(),
            queued: Vec::new(),
            rot_all: Rotation::Normal,
            rot_page: HashMap::new(),
            scroll: Vec2::ZERO,
            scroll_to_end: None,
            wheel_overscroll: 0.0,
            opening: None,
            info_rx: None,
            dialog: Dialog::None,
            show_menu: false,
            slideshow: None,
            toast: None,
            max_side,
            title: String::new(),
            last_view: Rect::NOTHING,
        };
        if let Some(p) = start.path {
            app.open_path(&cc.egui_ctx, p, start.page);
        } else if start.resume {
            app.run(&cc.egui_ctx, Cmd::Resume);
        } else {
            app.toast("Drop a comic here, press L to load, F1 for help, right-click for the menu");
        }
        app
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn page_count(&self) -> usize {
        self.book.as_ref().map_or(0, |b| b.pages.len())
    }

    fn rotation_of(&self, i: usize) -> Rotation {
        self.rot_all
            .compose(self.rot_page.get(&i).copied().unwrap_or_default())
    }

    fn is_wide(&self, i: usize) -> Option<bool> {
        if self.errors.contains_key(&i) {
            return Some(true); // a broken page is shown alone
        }
        self.pages.get(&i).map(|p| p.size.x > p.size.y)
    }

    fn spread(&self) -> Spread {
        layout::spread_at(self.page, self.page_count(), self.cfg.two_pages, |i| {
            self.is_wide(i)
        })
    }

    // ---------------------------------------------------------------- opening

    fn open_path(&mut self, ctx: &Context, path: PathBuf, page: Option<usize>) {
        self.remember_position();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let p = path.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Book::open(&p));
            c.request_repaint();
        });
        self.opening = Some((path, rx, page));
        self.slideshow = None;
    }

    fn poll_opening(&mut self, ctx: &Context) {
        let Some((path, rx, page)) = &self.opening else {
            return;
        };
        let Ok(result) = rx.try_recv() else { return };
        let (path, page) = (path.clone(), *page);
        self.opening = None;
        match result {
            Ok((book, start)) => {
                let book = Arc::new(book);
                let c = ctx.clone();
                self.loader = Some(Loader::new(book.clone(), move || c.request_repaint()));
                self.pages.clear();
                self.errors.clear();
                self.thumbs.clear();
                self.queued.clear();
                self.rot_page.clear();
                self.page = page
                    .unwrap_or(start)
                    .min(book.pages.len().saturating_sub(1));
                self.scroll_to_end = Some(false);
                if let Some(dir) = if book.path.is_dir() {
                    Some(book.path.as_path())
                } else {
                    book.path.parent()
                } {
                    self.cfg.last_dir = Some(dir.to_path_buf());
                }
                if self.cfg.display_info_files && !book.info.is_empty() {
                    self.spawn_info_reader(ctx, book.clone());
                }
                self.toast(format!(
                    "{} — {} pages ({})",
                    book.title,
                    book.pages.len(),
                    book.kind
                ));
                self.book = Some(book);
                if matches!(self.dialog, Dialog::Browser(_)) {
                    self.dialog = Dialog::None;
                }
                self.remember_position();
            }
            Err(e) => {
                log::error!("{}: {e:#}", path.display());
                self.dialog =
                    Dialog::Message(format!("Couldn't open {}:\n\n{e:#}", path.display()));
            }
        }
    }

    fn spawn_info_reader(&mut self, ctx: &Context, book: Arc<Book>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let c = ctx.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            for (i, e) in book.info.iter().enumerate() {
                if let Ok(t) = book.read_info(i) {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&format!("── {} ──\n{}", e.name, t.trim_end()));
                }
            }
            if !text.is_empty() {
                let _ = tx.send((book.title.clone(), text));
                c.request_repaint();
            }
        });
        self.info_rx = Some((rx, self.page));
    }

    fn remember_position(&mut self) {
        if let Some(b) = &self.book {
            let p = b.path.clone();
            self.cfg.remember(p, self.page);
            self.cfg.save();
        }
    }

    // ---------------------------------------------------------------- loading

    fn drain_results(&mut self, ctx: &Context) {
        let Some(loader) = &self.loader else { return };
        let results: Vec<_> = loader.results.try_iter().collect();
        let opts = if self.cfg.smooth_scaling {
            TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..TextureOptions::LINEAR
            }
        } else {
            TextureOptions::NEAREST
        };
        for r in results {
            let i = r.job.index;
            match (r.outcome, r.job.kind) {
                (Ok(p), JobKind::Page) => {
                    let size = [p.image.width() as usize, p.image.height() as usize];
                    let ci = ColorImage::from_rgba_unmultiplied(size, p.image.as_raw());
                    let tex = ctx.load_texture(format!("page-{i}"), ci, opts);
                    self.errors.remove(&i);
                    self.pages.insert(
                        i,
                        PageTex {
                            tex,
                            size: vec2(size[0] as f32, size[1] as f32),
                            rotation: r.job.rotation,
                            processing: r.job.processing,
                            info: PageInfo {
                                original_size: p.original_size,
                                format: p.format,
                                file_bytes: p.file_bytes,
                                balance: p.balance,
                                yellow_applied: p.yellow_applied,
                                edge_colour: p.edge_colour,
                                millis: p.millis,
                            },
                        },
                    );
                }
                (Ok(p), JobKind::Thumb) => {
                    let size = [p.image.width() as usize, p.image.height() as usize];
                    let ci = ColorImage::from_rgba_unmultiplied(size, p.image.as_raw());
                    let tex = ctx.load_texture(format!("thumb-{i}"), ci, TextureOptions::LINEAR);
                    self.thumbs.insert(
                        i,
                        Thumb {
                            tex,
                            rotation: r.job.rotation,
                        },
                    );
                }
                (Err(e), JobKind::Page) => {
                    log::warn!("page {}: {e}", i + 1);
                    self.errors.insert(i, e);
                }
                (Err(_), JobKind::Thumb) => {}
            }
        }
    }

    /// Decide which pages to decode next: the visible spread first, then the
    /// read-ahead buffer, then one page back, then thumbnails if they're shown.
    fn schedule(&mut self) {
        let Some(loader) = &self.loader else { return };
        let n = self.page_count();
        if n == 0 {
            return;
        }
        let processing = self.cfg.processing_key();
        let mut order: Vec<usize> = vec![self.page];
        if self.cfg.two_pages {
            order.push(self.page + 1);
        }
        let ahead = self.cfg.page_buffer.max(1) * if self.cfg.two_pages { 2 } else { 1 };
        order.extend((1..=ahead + 1).map(|d| self.page + d));
        order.push(self.page.wrapping_sub(1));
        if self.cfg.two_pages {
            order.push(self.page.wrapping_sub(2));
        }
        let mut seen = HashSet::new();
        order.retain(|&i| i < n && seen.insert(i));

        // Evict textures far from the reading position.
        let keep: HashSet<usize> = order.iter().copied().collect();
        self.pages.retain(|i, _| keep.contains(i));

        let active = loader.active();
        let mut jobs = Vec::new();
        for &i in &order {
            let rotation = self.rotation_of(i);
            let fresh = self
                .pages
                .get(&i)
                .is_some_and(|p| p.rotation == rotation && p.processing == processing);
            if fresh || self.errors.contains_key(&i) {
                continue;
            }
            let job = Job {
                index: i,
                kind: JobKind::Page,
                rotation,
                processing,
                max_side: self.max_side,
            };
            if !active.contains(&job) {
                jobs.push(job);
            }
        }
        let thumb_targets: Vec<usize> = match &self.dialog {
            Dialog::Thumbnails { .. } => {
                // nearest pages first so the visible part of the grid fills quickly
                let mut v: Vec<usize> = (0..n).collect();
                v.sort_by_key(|&i| (i as isize - self.page as isize).unsigned_abs());
                v
            }
            Dialog::Slider { value, .. } => vec![(*value).min(n - 1)],
            _ => Vec::new(),
        };
        for i in thumb_targets {
            let rotation = self.rotation_of(i);
            if self.thumbs.get(&i).is_some_and(|t| t.rotation == rotation) {
                continue;
            }
            let job = Job {
                index: i,
                kind: JobKind::Thumb,
                rotation,
                processing,
                max_side: 180,
            };
            if !active.contains(&job) {
                jobs.push(job);
            }
        }
        if jobs != self.queued {
            loader.set_queue(jobs.clone());
            self.queued = jobs;
        }
    }

    // ---------------------------------------------------------------- navigation

    fn goto(&mut self, page: usize, from_end: bool) {
        let n = self.page_count();
        if n == 0 {
            return;
        }
        let page = page.min(n - 1);
        if page != self.page {
            self.page = page;
            self.scroll_to_end = Some(from_end);
            self.wheel_overscroll = 0.0;
        }
    }

    fn next(&mut self) -> bool {
        let n = self.page_count();
        match layout::next_page(self.spread(), n, self.cfg.page_in_twos) {
            Some(p) => {
                self.goto(p, false);
                true
            }
            None => {
                if n > 0 {
                    self.toast("Last page — Shift+L loads the next file");
                }
                false
            }
        }
    }

    fn prev(&mut self) {
        let sp = self.spread();
        let back = layout::prev_page(sp, self.cfg.two_pages, self.cfg.page_back_in_twos, |i| {
            self.is_wide(i)
        });
        match back {
            Some(p) => self.goto(p, false),
            None if self.page_count() > 0 => {
                self.toast("First page — Shift+K loads the previous file")
            }
            None => {}
        }
    }

    // ---------------------------------------------------------------- commands

    fn run(&mut self, ctx: &Context, cmd: Cmd) {
        match cmd {
            Cmd::Open => {
                let start = self
                    .book
                    .as_ref()
                    .and_then(|b| b.path.parent().map(Path::to_path_buf))
                    .or_else(|| self.cfg.last_dir.clone())
                    .filter(|d| d.is_dir())
                    .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
                    .unwrap_or_else(|| PathBuf::from("/"));
                if self.cfg.use_system_file_dialog {
                    if let Some(p) = system_open_dialog(&start) {
                        self.open_path(ctx, p, None);
                    }
                } else {
                    self.dialog = Dialog::Browser(Browser::new(start));
                }
            }
            Cmd::OpenPath(p, page) => self.open_path(ctx, p, page),
            Cmd::NextFile | Cmd::PrevFile => {
                let fwd = cmd == Cmd::NextFile;
                match self
                    .book
                    .as_ref()
                    .and_then(|b| source::sibling(&b.path, fwd))
                {
                    Some(p) => self.open_path(ctx, p, None),
                    None => self.toast(if fwd {
                        "No next file in this folder"
                    } else {
                        "No previous file in this folder"
                    }),
                }
            }
            Cmd::Resume => match self.cfg.recent.first().cloned() {
                Some(r) if r.path.exists() => self.open_path(ctx, r.path, Some(r.page)),
                _ => self.toast("Nothing to resume"),
            },
            Cmd::Exit => {
                self.remember_position();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            Cmd::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
            Cmd::ToggleWindowed => {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fs));
            }
            Cmd::Next => {
                self.next();
            }
            Cmd::Prev => self.prev(),
            Cmd::First => self.goto(0, false),
            Cmd::Last => self.goto(self.page_count().saturating_sub(1), false),
            Cmd::Goto(p) => self.goto(p, false),
            Cmd::Thumbnails => {
                if self.book.is_some() {
                    self.dialog = Dialog::Thumbnails {
                        scroll_to_current: true,
                    };
                }
            }
            Cmd::Slider => {
                if self.book.is_some() {
                    self.dialog = Dialog::Slider {
                        value: self.page,
                        text: (self.page + 1).to_string(),
                    };
                }
            }
            Cmd::Slideshow => {
                self.slideshow = if self.slideshow.is_some() {
                    None
                } else {
                    Some(Instant::now())
                };
                self.toast(if self.slideshow.is_some() {
                    format!(
                        "Slideshow: every {:.0} s (A to stop)",
                        self.cfg.slideshow_seconds
                    )
                } else {
                    "Slideshow stopped".into()
                });
            }
            Cmd::Configure => self.dialog = Dialog::Config(Box::new(self.cfg.clone())),
            Cmd::RotatePage(r) => {
                let pages = self.spread().screen_order(false);
                for p in pages {
                    self.rot_page.insert(p, r);
                }
            }
            Cmd::RotateAll(r) => {
                self.rot_all = r;
                self.rot_page.clear();
            }
            Cmd::ToggleRotateAll(r) => {
                self.rot_all = if self.rot_all == r {
                    Rotation::Normal
                } else {
                    r
                };
                self.toast(format!("Rotate all: {:?}", self.rot_all));
            }
            Cmd::SaveToFile => self.save_page(),
            Cmd::CopyToClipboard => self.copy_page(),
            Cmd::Help => self.dialog = Dialog::Help,
            Cmd::About => self.dialog = Dialog::About,
            Cmd::ShowInfoFiles => match self.book.clone() {
                Some(b) if !b.info.is_empty() => self.spawn_info_reader(ctx, b),
                _ => self.toast("No info files (.txt/.nfo) in this book"),
            },
            Cmd::SetSize(m) => {
                self.cfg.size_mode = if self.cfg.size_mode == m {
                    SizeMode::Actual
                } else {
                    m
                };
                self.scroll_to_end = Some(false);
                self.toast(self.cfg.size_mode.label());
            }
            Cmd::ToggleTwoPages => {
                self.cfg.two_pages ^= true;
                self.toast(if self.cfg.two_pages {
                    "Two pages"
                } else {
                    "Single page"
                });
            }
            Cmd::ToggleJapanese => {
                self.cfg.japanese_mode ^= true;
                self.scroll_to_end = Some(false);
                self.toast(if self.cfg.japanese_mode {
                    "Japanese mode (right to left)"
                } else {
                    "Western mode (left to right)"
                });
            }
            Cmd::ToggleBalance => {
                self.cfg.colour_balance ^= true;
                self.toast(on_off("Colour balance", self.cfg.colour_balance));
            }
            Cmd::ToggleGamma => {
                self.cfg.gamma_correction ^= true;
                self.toast(format!(
                    "{} ({:.2})",
                    on_off("Gamma correction", self.cfg.gamma_correction),
                    self.cfg.gamma_x100 as f32 / 100.0
                ));
            }
            Cmd::ToggleYellow => {
                self.cfg.yellow_reduction ^= true;
                self.toast(on_off("Yellow reduction", self.cfg.yellow_reduction));
            }
            Cmd::ToggleInfo => self.cfg.image_information ^= true,
            Cmd::ToggleZoom => {
                self.cfg.zooming ^= true;
                self.toast(on_off("Magnifying glass", self.cfg.zooming));
            }
            Cmd::ToggleSmallCursor => self.cfg.small_cursor ^= true,
            Cmd::ToggleRotateScroll => {
                self.cfg.rotate_scroll_functions ^= true;
                self.toast(on_off(
                    "Rotate scroll functions",
                    self.cfg.rotate_scroll_functions,
                ));
            }
            Cmd::ToggleMenuBar => self.show_menu ^= true,
        }
    }

    fn current_page_name(&self) -> Option<(Arc<Book>, usize)> {
        let b = self.book.clone()?;
        (self.page < b.pages.len()).then_some((b, self.page))
    }

    /// "Save Page To File": writes the page's original file (no recompression).
    fn save_page(&mut self) {
        let Some((book, i)) = self.current_page_name() else {
            return;
        };
        let dir = directories::UserDirs::new()
            .and_then(|u| {
                u.picture_dir()
                    .map(Path::to_path_buf)
                    .or_else(|| Some(u.home_dir().to_path_buf()))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let leaf = Path::new(&book.pages[i].name)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = Path::new(&book.title)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let out = dir.join(format!("{stem} - {leaf}"));
        match book
            .read_page(i)
            .and_then(|b| std::fs::write(&out, b).map_err(Into::into))
        {
            Ok(()) => self.toast(format!("Saved {}", out.display())),
            Err(e) => self.toast(format!("Save failed: {e:#}")),
        }
    }

    /// "Save Page To Clipboard": the processed page as shown (filters applied).
    fn copy_page(&mut self) {
        let Some((book, i)) = self.current_page_name() else {
            return;
        };
        let job = Job {
            index: i,
            kind: JobKind::Page,
            rotation: self.rotation_of(i),
            processing: self.cfg.processing_key(),
            max_side: u32::MAX,
        };
        let r = crate::loader::run_job(&book, &job).and_then(|p| {
            let mut cb = arboard::Clipboard::new()?;
            cb.set_image(arboard::ImageData {
                width: p.image.width() as usize,
                height: p.image.height() as usize,
                bytes: std::borrow::Cow::Owned(p.image.into_raw()),
            })?;
            Ok(())
        });
        match r {
            Ok(()) => self.toast("Page copied to clipboard"),
            Err(e) => self.toast(format!("Clipboard: {e:#}")),
        }
    }

    // ---------------------------------------------------------------- input

    fn handle_keys(&mut self, ctx: &Context, view: Rect, max_scroll: Vec2) {
        match self.dialog {
            Dialog::None => {}
            // Read-only screens close on any key, like CDisplay's info display.
            Dialog::Info { .. } | Dialog::Help | Dialog::About | Dialog::Message(_) => {
                if ctx.input(|i| {
                    i.events
                        .iter()
                        .any(|e| matches!(e, Event::Key { pressed: true, .. }))
                }) {
                    self.dialog = Dialog::None;
                }
                return;
            }
            _ => {
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.dialog = Dialog::None;
                }
                return;
            }
        }
        let events = ctx.input(|i| i.events.clone());
        let mut cmds = Vec::new();
        for ev in events {
            match ev {
                // egui turns Ctrl+C into a Copy event; CDisplay used it for "small cursor".
                Event::Copy => cmds.push(Cmd::ToggleSmallCursor),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(c) = self.key_command(key, modifiers, view, max_scroll) {
                        cmds.push(c);
                    }
                }
                _ => {}
            }
        }
        for c in cmds {
            self.run(ctx, c);
        }
    }

    fn key_command(&mut self, key: Key, m: Modifiers, view: Rect, max: Vec2) -> Option<Cmd> {
        let ctrl = m.command || m.ctrl;
        let shift = m.shift;
        if ctrl {
            return Some(match (key, shift) {
                (Key::F, false) => Cmd::SetSize(SizeMode::Fit),
                (Key::F, true) => Cmd::SetSize(SizeMode::FitIfOversize),
                (Key::W, false) => Cmd::SetSize(SizeMode::FitWidth),
                (Key::W, true) => Cmd::SetSize(SizeMode::FitWidthIfOversize),
                (Key::H, false) => Cmd::SetSize(SizeMode::FitHeight),
                (Key::H, true) => Cmd::SetSize(SizeMode::FitHeightIfOversize),
                (Key::S, _) => Cmd::SetSize(SizeMode::TargetHeight),
                (Key::T, _) => Cmd::ToggleTwoPages,
                (Key::J, _) => Cmd::ToggleJapanese,
                (Key::L | Key::R, true) => Cmd::ToggleRotateScroll,
                (Key::L, false) => Cmd::ToggleRotateAll(Rotation::Left),
                (Key::R, false) => Cmd::ToggleRotateAll(Rotation::Right),
                (Key::B, _) => Cmd::ToggleBalance,
                (Key::G, _) => Cmd::ToggleGamma,
                (Key::Y, _) => Cmd::ToggleYellow,
                (Key::I, _) => Cmd::ToggleInfo,
                (Key::Z, _) => Cmd::ToggleZoom,
                (Key::C, _) => Cmd::ToggleSmallCursor,
                _ => return None,
            });
        }
        let step = vec2(view.width(), view.height()) * 0.15;
        let jp = self.cfg.japanese_mode;
        Some(match key {
            Key::PageDown => Cmd::Next,
            Key::PageUp => Cmd::Prev,
            Key::Home => Cmd::First,
            Key::End => Cmd::Last,
            Key::Space | Key::Enter => {
                if shift {
                    if self.scroll.y > 0.5 {
                        self.scroll.y -= view.height() * 0.9;
                        return None;
                    }
                    self.prev_from_end();
                    return None;
                }
                if self.scroll.y < max.y - 0.5 {
                    self.scroll.y += view.height() * 0.9;
                    return None;
                }
                // Then across: left-to-right, or right-to-left in Japanese mode.
                let across_left = if jp { self.scroll.x > 0.5 } else { false };
                let across_right = if jp {
                    false
                } else {
                    self.scroll.x < max.x - 0.5
                };
                if across_left || across_right {
                    self.scroll.x += if jp {
                        -view.width() * 0.9
                    } else {
                        view.width() * 0.9
                    };
                    self.scroll.y = 0.0;
                    return None;
                }
                Cmd::Next
            }
            Key::Backspace => Cmd::Prev,
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                let mut d = match key {
                    Key::ArrowUp => vec2(0.0, -step.y),
                    Key::ArrowDown => vec2(0.0, step.y),
                    Key::ArrowLeft => vec2(-step.x, 0.0),
                    _ => vec2(step.x, 0.0),
                };
                if self.cfg.rotate_scroll_functions {
                    d = rotate_vec(d, self.rot_all);
                }
                if d.x != 0.0 && max.x < 0.5 {
                    // Nothing to scroll sideways: turn the page instead.
                    let forward = (d.x > 0.0) != jp;
                    return Some(if forward { Cmd::Next } else { Cmd::Prev });
                }
                self.scroll += d;
                return None;
            }
            Key::L if shift => Cmd::NextFile,
            Key::K if shift => Cmd::PrevFile,
            Key::P if shift => Cmd::Slider,
            Key::L => Cmd::Open,
            Key::R => Cmd::Resume,
            Key::X | Key::Escape => Cmd::Exit,
            Key::M => Cmd::Minimize,
            Key::W => Cmd::ToggleWindowed,
            Key::P => Cmd::Thumbnails,
            Key::A => Cmd::Slideshow,
            Key::C => Cmd::Configure,
            Key::I => Cmd::ShowInfoFiles,
            Key::F1 => Cmd::Help,
            Key::F10 => Cmd::ToggleMenuBar,
            Key::F11 => Cmd::ToggleWindowed,
            _ => return None,
        })
    }

    fn prev_from_end(&mut self) {
        let before = self.page;
        self.prev();
        if self.page != before {
            self.scroll_to_end = Some(true);
        }
    }

    // ---------------------------------------------------------------- drawing

    fn draw_reader(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let view = ui.max_rect();
        self.last_view = view;
        let ppp = ctx.pixels_per_point();
        let resp = ui.allocate_rect(view, Sense::click_and_drag());
        let painter = ui.painter_at(view);

        let spread = self.spread();
        let order = spread.screen_order(self.cfg.japanese_mode);
        let ready: Vec<Shown> = order
            .iter()
            .filter_map(|&i| {
                self.pages.get(&i).map(|p| Shown {
                    index: i,
                    tex: p.tex.id(),
                    size: p.size,
                    edge: p.info.edge_colour,
                })
            })
            .collect();

        // Background
        let bg = match self.cfg.background {
            Background::Black => Color32::BLACK,
            Background::White => Color32::WHITE,
            Background::Gray => Color32::from_gray(128),
            Background::Auto => ready
                .first()
                .map(|p| Color32::from_rgb(p.edge[0], p.edge[1], p.edge[2]))
                .unwrap_or(Color32::BLACK),
        };
        painter.rect_filled(view, 0.0, bg);
        let fg = if bg.r() as u32 + bg.g() as u32 + bg.b() as u32 > 384 {
            Color32::BLACK
        } else {
            Color32::WHITE
        };

        let mut max_scroll = Vec2::ZERO;
        let mut image_rects: Vec<(Rect, Shown)> = Vec::new();

        if self.book.is_none() {
            let msg = if self.opening.is_some() {
                "Opening…"
            } else {
                "CDisplay\n\nDrop a comic or folder here, or press L to load.\nF1 for help."
            };
            painter.text(
                view.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(22.0),
                fg.gamma_multiply(0.7),
            );
        } else if ready.len() == order.len() && !ready.is_empty() {
            // Pages in a spread share a common height.
            let h = ready.iter().map(|p| p.size.y).fold(0.0, f32::max);
            let widths: Vec<f32> = ready.iter().map(|p| p.size.x * h / p.size.y).collect();
            let content = vec2(widths.iter().sum(), h);
            let mut mode = self.cfg.size_mode;
            if self.cfg.suppress_fit_width_for_double && ready.len() == 1 && content.x > content.y {
                mode = match mode {
                    SizeMode::FitWidth => SizeMode::Fit,
                    SizeMode::FitWidthIfOversize => SizeMode::FitIfOversize,
                    m => m,
                };
            }
            let view_px = (view.width() * ppp, view.height() * ppp);
            let s = layout::scale_for(
                mode,
                (content.x, content.y),
                view_px,
                self.cfg.target_height,
            ) / ppp;
            let disp = content * s;
            max_scroll = vec2(
                (disp.x - view.width()).max(0.0),
                (disp.y - view.height()).max(0.0),
            );

            if let Some(to_end) = self.scroll_to_end.take() {
                self.scroll.y = if to_end { max_scroll.y } else { 0.0 };
                self.scroll.x = if self.cfg.japanese_mode {
                    max_scroll.x
                } else {
                    0.0
                };
            }
            self.mouse_scroll(&ctx, &resp, max_scroll);
            self.scroll = self.scroll.clamp(Vec2::ZERO, max_scroll);

            let origin = pos2(
                if disp.x <= view.width() {
                    view.center().x - disp.x / 2.0
                } else {
                    view.left() - self.scroll.x
                },
                if disp.y <= view.height() {
                    view.center().y - disp.y / 2.0
                } else {
                    view.top() - self.scroll.y
                },
            );
            let mut x = origin.x;
            for (p, w) in ready.iter().zip(&widths) {
                let r = Rect::from_min_size(pos2(x.round(), origin.y.round()), vec2(w * s, h * s));
                painter.image(
                    p.tex,
                    r,
                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                image_rects.push((r, *p));
                x += w * s;
            }
        } else {
            let failed: Vec<String> = order
                .iter()
                .filter_map(|i| self.errors.get(i).map(|e| format!("Page {}: {e}", i + 1)))
                .collect();
            if failed.is_empty() {
                painter.text(
                    view.center(),
                    Align2::CENTER_CENTER,
                    format!("Loading page {}…", self.page + 1),
                    FontId::proportional(20.0),
                    fg.gamma_multiply(0.6),
                );
                self.mouse_scroll(&ctx, &resp, Vec2::ZERO);
            } else {
                painter.text(
                    view.center(),
                    Align2::CENTER_CENTER,
                    failed.join("\n"),
                    FontId::proportional(18.0),
                    Color32::from_rgb(230, 80, 80),
                );
                self.mouse_scroll(&ctx, &resp, Vec2::ZERO);
            }
        }

        // Magnifying glass (only when the whole page is visible, as in the original).
        if self.cfg.zooming
            && max_scroll == Vec2::ZERO
            && !resp.dragged()
            && let Some(ptr) = resp.hover_pos()
            && image_rects.iter().any(|(r, _)| r.contains(ptr))
        {
            self.draw_magnifier(&painter, view, ptr, &image_rects, bg);
        }

        if self.cfg.image_information && self.book.is_some() {
            self.draw_info(&painter, view, &image_rects);
        }

        if self.cfg.small_cursor && resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::None);
            if let Some(p) = resp.hover_pos() {
                painter.circle_filled(p, 2.5, fg);
            }
        } else if resp.dragged() {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        }

        self.handle_clicks(&ctx, &resp, view);
        resp.context_menu(|ui| {
            let cmds = self.menu_items(ui);
            for c in cmds {
                self.run(&ctx, c);
            }
        });
        self.handle_keys(&ctx, view, max_scroll);
    }

    fn mouse_scroll(&mut self, ctx: &Context, resp: &egui::Response, max: Vec2) {
        if self.cfg.left_click == ClickAction::DragImage && resp.dragged_by(PointerButton::Primary)
        {
            self.scroll -= resp.drag_delta();
        }
        if !resp.hovered() {
            return;
        }
        let delta = ctx.input(|i| i.smooth_scroll_delta);
        if delta == Vec2::ZERO {
            if ctx.input(|i| i.pointer.any_down()) {
                self.wheel_overscroll = 0.0;
            }
            return;
        }
        match self.cfg.wheel {
            WheelAction::None => {}
            WheelAction::TurnPage => {
                self.wheel_overscroll += delta.y;
                if self.wheel_overscroll < -40.0 {
                    self.wheel_overscroll = 0.0;
                    self.next();
                } else if self.wheel_overscroll > 40.0 {
                    self.wheel_overscroll = 0.0;
                    self.prev();
                }
            }
            WheelAction::ScrollPage => {
                let at_bottom = self.scroll.y >= max.y - 0.5;
                let at_top = self.scroll.y <= 0.5;
                if (delta.y < 0.0 && at_bottom) || (delta.y > 0.0 && at_top) {
                    // Keep turning past the edge to change page.
                    self.wheel_overscroll += delta.y;
                    if self.wheel_overscroll < -120.0 {
                        self.wheel_overscroll = 0.0;
                        self.next();
                    } else if self.wheel_overscroll > 120.0 {
                        self.wheel_overscroll = 0.0;
                        self.prev_from_end();
                    }
                } else {
                    self.wheel_overscroll = 0.0;
                    self.scroll -= delta;
                }
            }
        }
    }

    fn handle_clicks(&mut self, ctx: &Context, resp: &egui::Response, view: Rect) {
        if self.cfg.right_double_click_next && resp.double_clicked_by(PointerButton::Secondary) {
            egui::Popup::close_all(ctx);
            self.next();
            return;
        }
        if resp.clicked_by(PointerButton::Primary) {
            match self.cfg.left_click {
                ClickAction::DragImage | ClickAction::None => {}
                ClickAction::NextPage => {
                    self.next();
                }
                ClickAction::PreviousPage => self.prev(),
                ClickAction::NextOrPrevious => {
                    let right = resp
                        .interact_pointer_pos()
                        .is_some_and(|p| p.x > view.center().x);
                    if right != self.cfg.japanese_mode {
                        self.next();
                    } else {
                        self.prev();
                    }
                }
            }
        }
        if resp.clicked_by(PointerButton::Middle) {
            self.next();
        }
    }

    fn draw_magnifier(
        &self,
        painter: &egui::Painter,
        view: Rect,
        ptr: Pos2,
        rects: &[(Rect, Shown)],
        bg: Color32,
    ) {
        let zf = self.cfg.zoom_factor.max(1.1);
        let side = (view.height().min(view.width()) * 0.38).max(120.0);
        let lens = Rect::from_center_size(ptr, vec2(side, side));
        let lp = painter.with_clip_rect(lens.intersect(view));
        lp.rect_filled(lens, 0.0, bg);
        let src = Rect::from_center_size(ptr, lens.size() / zf);
        for (r, p) in rects {
            let part = src.intersect(*r);
            if !part.is_positive() {
                continue;
            }
            let uv = Rect::from_min_max(
                pos2(
                    (part.min.x - r.min.x) / r.width(),
                    (part.min.y - r.min.y) / r.height(),
                ),
                pos2(
                    (part.max.x - r.min.x) / r.width(),
                    (part.max.y - r.min.y) / r.height(),
                ),
            );
            let dest = Rect::from_min_max(ptr + (part.min - ptr) * zf, ptr + (part.max - ptr) * zf);
            lp.image(p.tex, dest, uv, Color32::WHITE);
        }
        painter.rect_stroke(
            lens,
            0.0,
            egui::Stroke::new(2.0, Color32::from_gray(90)),
            egui::StrokeKind::Outside,
        );
    }

    fn draw_info(&self, painter: &egui::Painter, view: Rect, rects: &[(Rect, Shown)]) {
        let Some(book) = &self.book else { return };
        let mut lines = vec![
            format!("{}  [{}]", book.title, book.kind),
            match self.spread().second {
                Some(s) => format!("Pages {}–{} of {}", self.page + 1, s + 1, book.pages.len()),
                None => format!("Page {} of {}", self.page + 1, book.pages.len()),
            },
        ];
        let order = self.spread().screen_order(false);
        for i in order {
            let name = &book.pages[i].name;
            match self.pages.get(&i) {
                Some(p) => {
                    let orig_h = if p.rotation.swaps_axes() {
                        p.info.original_size.0
                    } else {
                        p.info.original_size.1
                    }
                    .max(1) as f32;
                    let shown = rects
                        .iter()
                        .find(|(_, t)| t.index == i)
                        .map(|(r, _)| r.height() * painter.ctx().pixels_per_point() / orig_h);
                    let mut s = format!(
                        "{name}: {} {}×{}, {}  {}ms",
                        p.info.format,
                        p.info.original_size.0,
                        p.info.original_size.1,
                        human_bytes(p.info.file_bytes),
                        p.info.millis
                    );
                    if let Some(z) = shown {
                        s.push_str(&format!("  shown at {:.0}%", z * 100.0));
                    }
                    match p.info.balance {
                        Some(BalanceOutcome::Applied(c)) => {
                            s.push_str(&format!("  balance: contrast {c}%"))
                        }
                        Some(BalanceOutcome::LowRange) => {
                            s.push_str("  balance: skipped (low range)")
                        }
                        None => {}
                    }
                    match p.info.yellow_applied {
                        Some(true) => s.push_str("  reduced yellowing"),
                        Some(false) => s.push_str("  yellowing: page already white"),
                        None => {}
                    }
                    if p.rotation != Rotation::Normal {
                        s.push_str(&format!("  rotated {:?}", p.rotation));
                    }
                    lines.push(s);
                }
                None => lines.push(format!("{name}: loading…")),
            }
        }
        lines.push(format!(
            "{}{}",
            self.cfg.size_mode.label(),
            if self.cfg.japanese_mode {
                "  ·  Japanese mode"
            } else {
                ""
            }
        ));
        let text = lines.join("\n");
        let galley = painter.layout_no_wrap(text, FontId::monospace(13.0), Color32::WHITE);
        let r = Rect::from_min_size(view.min + vec2(8.0, 8.0), galley.size() + vec2(16.0, 12.0));
        painter.rect_filled(r, 6.0, Color32::from_black_alpha(180));
        painter.galley(r.min + vec2(8.0, 6.0), galley, Color32::WHITE);
    }

    fn draw_toast(&mut self, ctx: &Context) {
        let Some((msg, t)) = &self.toast else { return };
        let age = t.elapsed().as_secs_f32();
        if age > 3.0 {
            self.toast = None;
            return;
        }
        let alpha = ((3.0 - age) / 0.5).clamp(0.0, 1.0);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("toast"),
        ));
        let view = ctx.content_rect();
        let galley = painter.layout_no_wrap(
            msg.clone(),
            FontId::proportional(16.0),
            Color32::WHITE.gamma_multiply(alpha),
        );
        let r = Rect::from_center_size(
            pos2(view.center().x, view.bottom() - 48.0),
            galley.size() + vec2(28.0, 16.0),
        );
        painter.rect_filled(r, 8.0, Color32::from_black_alpha((190.0 * alpha) as u8));
        painter.galley(r.min + vec2(14.0, 8.0), galley, Color32::WHITE);
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    // ---------------------------------------------------------------- menus

    /// The popup / main menu (File, Read, Options, Help), mirroring the original.
    fn menu_items(&self, ui: &mut egui::Ui) -> Vec<Cmd> {
        let mut out = Vec::new();
        let mut item = |ui: &mut egui::Ui, label: &str, c: Cmd| {
            if ui.button(label).clicked() {
                out.push(c);
                ui.close();
            }
        };
        ui.menu_button("File", |ui| {
            item(ui, "Load Files (L)", Cmd::Open);
            item(ui, "Load Next Files (Shift+L)", Cmd::NextFile);
            item(ui, "Load Previous Files (Shift+K)", Cmd::PrevFile);
            item(ui, "Resume Reading (R)", Cmd::Resume);
            ui.menu_button("History", |ui| {
                if self.cfg.recent.is_empty() {
                    ui.label("(empty)");
                }
                for r in &self.cfg.recent {
                    let name = r
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    item(
                        ui,
                        &format!("{name}  — p.{}", r.page + 1),
                        Cmd::OpenPath(r.path.clone(), Some(r.page)),
                    );
                }
            });
            ui.separator();
            item(ui, "Automatic Slideshow (A)", Cmd::Slideshow);
            item(ui, "Show Info Files (I)", Cmd::ShowInfoFiles);
            ui.separator();
            item(ui, "Save Page To File", Cmd::SaveToFile);
            item(ui, "Save Page To Clipboard", Cmd::CopyToClipboard);
            ui.separator();
            item(ui, "Minimize (M)", Cmd::Minimize);
            item(ui, "Windowed / Full Screen (W)", Cmd::ToggleWindowed);
            item(ui, "Exit (X)", Cmd::Exit);
        });
        ui.menu_button("Read", |ui| {
            item(ui, "Next Page (Page Down)", Cmd::Next);
            item(ui, "Previous Page (Page Up)", Cmd::Prev);
            item(ui, "First Page (Home)", Cmd::First);
            item(ui, "Last Page (End)", Cmd::Last);
            ui.separator();
            item(ui, "Go To Page - Thumbnails (P)", Cmd::Thumbnails);
            item(ui, "Go To Page - Slider (Shift+P)", Cmd::Slider);
        });
        ui.menu_button("Options", |ui| {
            ui.menu_button("Image Size", |ui| {
                for m in SizeMode::ALL {
                    let mark = if self.cfg.size_mode == m {
                        "● "
                    } else {
                        "   "
                    };
                    item(ui, &format!("{mark}{}", m.label()), Cmd::SetSize(m));
                }
            });
            let check = |b: bool| if b { "✔ " } else { "   " };
            item(
                ui,
                &format!("{}Two Pages (Ctrl+T)", check(self.cfg.two_pages)),
                Cmd::ToggleTwoPages,
            );
            item(
                ui,
                &format!("{}Japanese Mode (Ctrl+J)", check(self.cfg.japanese_mode)),
                Cmd::ToggleJapanese,
            );
            item(
                ui,
                &format!("{}Colour Balance (Ctrl+B)", check(self.cfg.colour_balance)),
                Cmd::ToggleBalance,
            );
            item(
                ui,
                &format!(
                    "{}Gamma Correction (Ctrl+G)",
                    check(self.cfg.gamma_correction)
                ),
                Cmd::ToggleGamma,
            );
            item(
                ui,
                &format!(
                    "{}Yellow Reduction (Ctrl+Y)",
                    check(self.cfg.yellow_reduction)
                ),
                Cmd::ToggleYellow,
            );
            item(
                ui,
                &format!(
                    "{}Image Information (Ctrl+I)",
                    check(self.cfg.image_information)
                ),
                Cmd::ToggleInfo,
            );
            item(
                ui,
                &format!("{}Magnifying Glass (Ctrl+Z)", check(self.cfg.zooming)),
                Cmd::ToggleZoom,
            );
            ui.menu_button("Rotate", |ui| {
                ui.menu_button("This Page", |ui| {
                    for r in [
                        Rotation::Normal,
                        Rotation::Left,
                        Rotation::Right,
                        Rotation::Invert,
                    ] {
                        item(ui, &format!("{r:?}"), Cmd::RotatePage(r));
                    }
                });
                ui.menu_button("All Images", |ui| {
                    for r in [
                        Rotation::Normal,
                        Rotation::Left,
                        Rotation::Right,
                        Rotation::Invert,
                    ] {
                        item(ui, &format!("{r:?}"), Cmd::RotateAll(r));
                    }
                });
            });
            ui.separator();
            item(ui, "Configure… (C)", Cmd::Configure);
        });
        ui.menu_button("Help", |ui| {
            item(ui, "Keys (F1)", Cmd::Help);
            item(ui, "About", Cmd::About);
        });
        out
    }

    // ---------------------------------------------------------------- dialogs

    fn draw_dialogs(&mut self, ctx: &Context) {
        let mut cmds = Vec::new();
        let mut close = false;
        let screen = ctx.content_rect();
        let n = self.page_count();
        match &mut self.dialog {
            Dialog::None => {}
            Dialog::Help => {
                egui::Window::new("CDisplay keys")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(screen.height() * 0.8)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(HELP_TEXT).monospace());
                            });
                        close |= ui.button("Close").clicked();
                    });
            }
            Dialog::About => {
                egui::Window::new("About CDisplay").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
                    ui.heading(format!("CDisplay-rs {}", env!("CARGO_PKG_VERSION")));
                    ui.label("A Rust port of CDisplay Image Display, the sequential image viewer by David Ayton (1.8, 2004).");
                    ui.label("Rebuilt from the original program's behaviour and settings, with its colour balance, gamma and yellow-reduction filters reproduced from the binary.");
                    ui.add_space(6.0);
                    ui.label("Formats: JPEG, PNG, GIF, BMP, WebP, JPEG XL, TIFF, TGA, ICO, PNM, QOI");
                    ui.label("Archives: CBZ/ZIP, CBR/RAR, CB7/7z, CBT/TAR, CBA/ACE (via unar), folders");
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("In memory of David Ayton.").italics());
                    close |= ui.button("OK").clicked();
                });
            }
            Dialog::Message(msg) => {
                egui::Window::new("CDisplay")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label(msg.as_str());
                        close |= ui.button("OK").clicked();
                    });
            }
            Dialog::Info { title, text } => {
                egui::Window::new(format!("Info — {title}"))
                    .collapsible(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_size([screen.width() * 0.6, screen.height() * 0.7])
                    .show(ctx, |ui| {
                        egui::ScrollArea::both()
                            .max_height(screen.height() * 0.7)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(text.as_str()).monospace());
                            });
                        close |= ui.button("Close (Esc)").clicked();
                    });
            }
            Dialog::Slider { value, text } => {
                egui::Window::new("Go To Page")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ctx, |ui| {
                        if let Some(t) = self.thumbs.get(value) {
                            let s = t.tex.size_vec2();
                            ui.vertical_centered(|ui| {
                                ui.image(egui::load::SizedTexture::new(t.tex.id(), s))
                            });
                        } else {
                            ui.allocate_space(vec2(120.0, 180.0));
                        }
                        let mut v = *value + 1;
                        if ui
                            .add(egui::Slider::new(&mut v, 1..=n.max(1)).text("page"))
                            .changed()
                        {
                            *value = v - 1;
                            *text = v.to_string();
                        }
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(text).desired_width(60.0));
                            if r.changed()
                                && let Ok(p) = text.trim().parse::<usize>()
                            {
                                *value = p.clamp(1, n.max(1)) - 1;
                            }
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if ui.button("Go").clicked() || enter {
                                cmds.push(Cmd::Goto(*value));
                                close = true;
                            }
                            close |= ui.button("Cancel").clicked();
                        });
                    });
            }
            Dialog::Thumbnails { scroll_to_current } => {
                let mut stc = *scroll_to_current;
                egui::Window::new("Go To Page — Thumbnails")
                    .collapsible(false)
                    .resizable(false)
                    .fixed_rect(screen.shrink(screen.width().min(screen.height()) * 0.04))
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for i in 0..n {
                                        let resp = ui
                                            .allocate_ui(vec2(132.0, 210.0), |ui| {
                                                ui.vertical_centered(|ui| {
                                                    let r = match self.thumbs.get(&i) {
                                                        Some(t) => {
                                                            let s = t.tex.size_vec2();
                                                            let fit = (120.0 / s.x)
                                                                .min(180.0 / s.y)
                                                                .min(1.0);
                                                            ui.add(
                                                                egui::Button::image(
                                                                    egui::load::SizedTexture::new(
                                                                        t.tex.id(),
                                                                        s * fit,
                                                                    ),
                                                                )
                                                                .selected(i == self.page),
                                                            )
                                                        }
                                                        None => ui.add_sized(
                                                            [120.0, 180.0],
                                                            egui::Button::new("…")
                                                                .selected(i == self.page),
                                                        ),
                                                    };
                                                    ui.label((i + 1).to_string());
                                                    r
                                                })
                                                .inner
                                            })
                                            .inner;
                                        if i == self.page && stc {
                                            resp.scroll_to_me(Some(egui::Align::Center));
                                            stc = false;
                                        }
                                        if resp.clicked() {
                                            cmds.push(Cmd::Goto(i));
                                            close = true;
                                        }
                                    }
                                });
                            });
                    });
                *scroll_to_current = stc;
            }
            Dialog::Browser(b) => {
                let mut nav: Option<PathBuf> = None;
                egui::Window::new("Load Files")
                    .collapsible(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_size([screen.width().min(760.0), screen.height() * 0.75])
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("⬆ Up").clicked() {
                                nav = b.dir.parent().map(Path::to_path_buf);
                            }
                            if ui.button("🏠").on_hover_text("Home").clicked() {
                                nav = directories::UserDirs::new()
                                    .map(|u| u.home_dir().to_path_buf());
                            }
                            ui.label(b.dir.display().to_string());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Filter:");
                            ui.text_edit_singleline(&mut b.filter);
                        });
                        ui.separator();
                        let f = b.filter.to_lowercase();
                        egui::ScrollArea::vertical()
                            .max_height(screen.height() * 0.55)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (name, path, is_dir) in &b.entries {
                                    if !f.is_empty() && !name.to_lowercase().contains(&f) {
                                        continue;
                                    }
                                    let icon = if *is_dir {
                                        "📁"
                                    } else if source::is_archive_name(name) {
                                        "📚"
                                    } else {
                                        "🖼"
                                    };
                                    let r = ui.selectable_label(false, format!("{icon} {name}"));
                                    if r.clicked() {
                                        if *is_dir {
                                            nav = Some(path.clone());
                                        } else {
                                            cmds.push(Cmd::OpenPath(path.clone(), None));
                                        }
                                    }
                                    if *is_dir {
                                        r.context_menu(|ui| {
                                            if ui.button("Open as book").clicked() {
                                                cmds.push(Cmd::OpenPath(path.clone(), None));
                                                ui.close();
                                            }
                                        });
                                    }
                                }
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Read this folder").clicked() {
                                cmds.push(Cmd::OpenPath(b.dir.clone(), None));
                            }
                            if ui.button("System dialog…").clicked()
                                && let Some(p) = system_open_dialog(&b.dir)
                            {
                                cmds.push(Cmd::OpenPath(p, None));
                            }
                            close |= ui.button("Cancel").clicked();
                        });
                    });
                if let Some(d) = nav {
                    b.dir = d;
                    b.filter.clear();
                    b.refresh();
                }
            }
            Dialog::Config(draft) => {
                let mut action = None;
                egui::Window::new("Configure")
                    .collapsible(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(560.0)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(screen.height() * 0.75)
                            .show(ui, |ui| config_ui(ui, draft));
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                action = Some("ok");
                            }
                            if ui.button("Apply").clicked() {
                                action = Some("apply");
                            }
                            if ui.button("Default").clicked() {
                                action = Some("default");
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some("cancel");
                            }
                        });
                    });
                match action {
                    Some("default") => {
                        let keep = (draft.recent.clone(), draft.last_dir.clone());
                        **draft = Config {
                            recent: keep.0,
                            last_dir: keep.1,
                            ..Config::default()
                        };
                    }
                    Some("apply") | Some("ok") => {
                        self.cfg = (**draft).clone();
                        self.cfg.save();
                        close = action == Some("ok");
                    }
                    Some(_) => close = true,
                    None => {}
                }
            }
        }
        if close {
            self.dialog = Dialog::None;
        }
        for c in cmds {
            self.run(ctx, c);
        }
    }
}

fn config_ui(ui: &mut egui::Ui, c: &mut Config) {
    ui.collapsing("Image Sizing", |ui| {
        for m in SizeMode::ALL {
            ui.radio_value(&mut c.size_mode, m, m.label());
        }
        ui.add(egui::Slider::new(&mut c.target_height, 1..=4096).text("Set Height (pixels)"));
        ui.checkbox(&mut c.two_pages, "Display Two Pages (Ctrl+T)");
        ui.checkbox(&mut c.page_in_twos, "Forward two pages in two-page mode");
        ui.checkbox(
            &mut c.page_back_in_twos,
            "Backward two pages in two-page mode",
        );
        ui.checkbox(
            &mut c.suppress_fit_width_for_double,
            "Suppress Fit Width for double pages",
        );
        ui.checkbox(&mut c.japanese_mode, "Japanese Mode (Ctrl+J)");
        ui.checkbox(
            &mut c.rotate_scroll_functions,
            "Rotate scroll functions with the image",
        );
        ui.checkbox(&mut c.smooth_scaling, "Smooth scaling");
    })
    .header_response
    .on_hover_text("All options keep the pages' aspect ratios");
    ui.collapsing("Colour Balance", |ui| {
        ui.checkbox(&mut c.colour_balance, "Colour Balance On (Ctrl+B)");
        ui.add(egui::Slider::new(&mut c.discards_per_1000, 0..=1000).text("Discards per 1000"));
        ui.checkbox(&mut c.gamma_correction, "Gamma Correction (Ctrl+G)");
        let mut g = c.gamma_x100 as f32 / 100.0;
        if ui
            .add(
                egui::Slider::new(&mut g, 0.01..=7.0)
                    .text("Gamma")
                    .step_by(0.01),
            )
            .changed()
        {
            c.gamma_x100 = (g * 100.0).round() as i32;
        }
    });
    ui.collapsing("Yellow Reduction", |ui| {
        ui.label("Detects pale yellow (aged paper) and reduces it — can distort colours if set too high.");
        ui.checkbox(&mut c.yellow_reduction, "Yellow Reduction On (Ctrl+Y)");
        ui.add(egui::Slider::new(&mut c.yellow.red_threshold, 0..=255).text("Red threshold"));
        ui.add(egui::Slider::new(&mut c.yellow.green_threshold, 0..=255).text("Green threshold"));
        ui.add(egui::Slider::new(&mut c.yellow.blue_threshold, 0..=255).text("Blue threshold"));
        ui.add(egui::Slider::new(&mut c.yellow.green_boost, 0..=255).text("Green boost"));
        ui.add(egui::Slider::new(&mut c.yellow.blue_boost, 0..=255).text("Blue boost"));
    });
    ui.collapsing("Edge Yellow Reduction", |ui| {
        ui.label("Aggressive, experimental: repaints yellowed paper connected to the page edge. Can leak into panels without borders.");
        ui.checkbox(&mut c.edge_yellow_reduction, "Edge Yellow Reduction On");
        ui.checkbox(&mut c.internal_yellow_reduction, "Internal Yellow Reduction On");
        ui.add(egui::Slider::new(&mut c.edge_yellow.red_threshold, 0..=255).text("Red threshold"));
        ui.add(egui::Slider::new(&mut c.edge_yellow.green_threshold, 0..=255).text("Green threshold"));
        ui.add(egui::Slider::new(&mut c.edge_yellow.blue_threshold, 0..=255).text("Blue threshold"));
        ui.add(egui::Slider::new(&mut c.edge_yellow.red_setting, 0..=255).text("Red setting"));
        ui.add(egui::Slider::new(&mut c.edge_yellow.green_setting, 0..=255).text("Green setting"));
        ui.add(egui::Slider::new(&mut c.edge_yellow.blue_setting, 0..=255).text("Blue setting"));
    });
    ui.collapsing("Program Settings", |ui| {
        egui::ComboBox::from_label("Background colour")
            .selected_text(format!("{:?}", c.background))
            .show_ui(ui, |ui| {
                for b in [
                    Background::Auto,
                    Background::Black,
                    Background::White,
                    Background::Gray,
                ] {
                    ui.selectable_value(&mut c.background, b, format!("{b:?}"));
                }
            });
        ui.checkbox(&mut c.image_information, "Image Information (Ctrl+I)");
        ui.checkbox(&mut c.zooming, "Magnifying glass (Ctrl+Z)");
        ui.add(egui::Slider::new(&mut c.zoom_factor, 1.5..=6.0).text("Magnification"));
        ui.checkbox(&mut c.small_cursor, "Small cursor (Ctrl+C)");
        ui.checkbox(
            &mut c.display_info_files,
            "Display info files (.txt/.nfo) when opening",
        );
        ui.add(egui::Slider::new(&mut c.page_buffer, 1..=8).text("Page buffer (pages read ahead)"));
        ui.add(egui::Slider::new(&mut c.slideshow_seconds, 1.0..=60.0).text("Slideshow seconds"));
        ui.checkbox(
            &mut c.use_system_file_dialog,
            "Use the desktop's file dialog",
        );
        ui.checkbox(&mut c.start_fullscreen, "Start full screen");
    });
    ui.collapsing("Mouse Control", |ui| {
        egui::ComboBox::from_label("Left click")
            .selected_text(c.left_click.label())
            .show_ui(ui, |ui| {
                for a in ClickAction::ALL {
                    ui.selectable_value(&mut c.left_click, a, a.label());
                }
            });
        ui.checkbox(
            &mut c.right_double_click_next,
            "Right double-click: next page",
        );
        egui::ComboBox::from_label("Mouse wheel")
            .selected_text(format!("{:?}", c.wheel))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut c.wheel, WheelAction::ScrollPage, "Scroll Page");
                ui.selectable_value(&mut c.wheel, WheelAction::TurnPage, "Turn Page");
                ui.selectable_value(&mut c.wheel, WheelAction::None, "None");
            });
        ui.label("Middle click: next page. Right click: menu.");
    });
}

fn system_open_dialog(start: &Path) -> Option<PathBuf> {
    let mut exts: Vec<&str> = source::ARCHIVE_EXTS.to_vec();
    exts.extend_from_slice(source::IMAGE_EXTS);
    rfd::FileDialog::new()
        .set_directory(start)
        .add_filter("Comics and images", &exts)
        .add_filter("Comic archives", source::ARCHIVE_EXTS)
        .add_filter("Images", source::IMAGE_EXTS)
        .pick_file()
}

fn rotate_vec(v: Vec2, r: Rotation) -> Vec2 {
    match r {
        Rotation::Normal => v,
        Rotation::Right => vec2(-v.y, v.x),
        Rotation::Invert => -v,
        Rotation::Left => vec2(v.y, -v.x),
    }
}

fn on_off(name: &str, on: bool) -> String {
    format!("{name} {}", if on { "on" } else { "off" })
}

fn human_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} KB", n as f64 / 1024.0)
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_opening(&ctx);
        self.drain_results(&ctx);

        if let Some((rx, page)) = &self.info_rx
            && let Ok((title, text)) = rx.try_recv()
        {
            // Don't interrupt someone who has already started reading.
            if matches!(self.dialog, Dialog::None) && *page == self.page {
                self.dialog = Dialog::Info { title, text };
            }
            self.info_rx = None;
        }

        // Drag and drop
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .first()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
        });
        if let Some(p) = dropped {
            self.open_path(&ctx, p, None);
        }

        if self.show_menu {
            egui::Panel::top("menu").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    let cmds = self.menu_items(ui);
                    for c in cmds {
                        self.run(&ctx, c);
                    }
                });
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.draw_reader(ui));
        self.draw_dialogs(&ctx);
        self.draw_toast(&ctx);
        self.schedule();

        // Slideshow
        if let Some(t) = self.slideshow {
            let period = Duration::from_secs_f32(self.cfg.slideshow_seconds.max(0.5));
            if t.elapsed() >= period {
                if self.next() {
                    self.slideshow = Some(Instant::now());
                } else {
                    self.slideshow = None;
                }
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        let title = match &self.book {
            Some(b) => format!(
                "CDisplay — {} — {}/{}",
                b.title,
                self.page + 1,
                b.pages.len()
            ),
            None => "CDisplay".to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.opening.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.remember_position();
    }
}
