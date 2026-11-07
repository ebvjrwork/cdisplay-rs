//! CDisplay-rs — a Rust port of David Ayton's CDisplay sequential image viewer.

mod app;
mod config;
mod decode;
mod filters;
mod layout;
mod loader;
mod source;

use std::path::PathBuf;

const USAGE: &str = "\
cdisplay-rs [OPTIONS] [FILE_OR_FOLDER]

Opens a comic archive (cbz/zip, cbr/rar, cb7/7z, cbt/tar, cba/ace), a folder
of images, or a single image.

Options:
  -p, --page N        start at page N (1-based)
  -w, --windowed      start in a window instead of full screen
  -r, --resume        reopen the last book at the last page read
      --export N OUT  headless: write page N, processed with the saved
                      colour settings, to image file OUT, then exit
  -h, --help          show this help
";

struct Args {
    path: Option<PathBuf>,
    page: Option<usize>,
    windowed: bool,
    resume: bool,
    export: Option<(usize, PathBuf)>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        path: None,
        page: None,
        windowed: false,
        resume: false,
        export: None,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            Some("-w" | "--windowed") => a.windowed = true,
            Some("-r" | "--resume") => a.resume = true,
            Some("-p" | "--page") => {
                let n = it
                    .next()
                    .and_then(|s| s.to_str()?.parse::<usize>().ok())
                    .ok_or("--page needs a number")?;
                a.page = Some(n.saturating_sub(1));
            }
            Some("--export") => {
                let n = it
                    .next()
                    .and_then(|s| s.to_str()?.parse::<usize>().ok())
                    .ok_or("--export needs a page number")?;
                let out = it.next().ok_or("--export needs an output file")?;
                a.export = Some((n.saturating_sub(1), out.into()));
            }
            Some(s) if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option {s}\n\n{USAGE}"));
            }
            _ => a.path = Some(arg.into()),
        }
    }
    Ok(a)
}

fn export(path: &std::path::Path, page: usize, out: &std::path::Path) -> anyhow::Result<()> {
    let cfg = config::Config::load();
    let (book, _) = source::Book::open(path)?;
    let job = loader::Job {
        index: page,
        kind: loader::JobKind::Page,
        rotation: layout::Rotation::Normal,
        processing: cfg.processing_key(),
        max_side: u32::MAX,
    };
    let p = loader::run_job(&book, &job)?;
    p.image.save(out)?;
    println!(
        "{} page {}/{} ({}, {}x{}) -> {}",
        book.title,
        page + 1,
        book.pages.len(),
        p.format,
        p.original_size.0,
        p.original_size.1,
        out.display()
    );
    Ok(())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    if let Some((page, out)) = &args.export {
        let Some(path) = &args.path else {
            eprintln!("--export needs a FILE_OR_FOLDER");
            std::process::exit(2);
        };
        if let Err(e) = export(path, *page, out) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    let cfg = config::Config::load();
    let fullscreen = cfg.start_fullscreen && !args.windowed;
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("CDisplay")
        .with_inner_size([1100.0, 800.0])
        .with_min_inner_size([320.0, 240.0])
        .with_fullscreen(fullscreen)
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let start = app::StartRequest {
        path: args.path,
        page: args.page,
        resume: args.resume,
    };
    if let Err(e) = eframe::run_native(
        "CDisplay",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg, start)))),
    ) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
