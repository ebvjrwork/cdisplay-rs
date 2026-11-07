# CDisplay-rs

A Rust port of **CDisplay**, the sequential image ("comic book") viewer written by
the late David Ayton (version 1.8, April 2004). It keeps the original's keyboard-first,
full-screen reading style and settings, and adds modern image formats such as **WebP**
and **JPEG XL**. It runs on Linux, and builds on Windows and macOS.

## Formats

| | Original CDisplay 1.8 | CDisplay-rs |
|---|---|---|
| Images | JPEG, PNG, GIF, BMP | JPEG, PNG, GIF, BMP, **WebP**, **JPEG XL**, TIFF, TGA, ICO, PNM, QOI (+ AVIF with `--features avif`) |
| Archives | ZIP/CBZ (UnzDll.dll), RAR/CBR (UnRar.dll), ACE/CBA (UNACEV2.DLL), TAR/CBT | ZIP/CBZ, RAR/CBR (built-in), **7z/CB7**, TAR/CBT, ACE/CBA via the external `unar` tool |
| Info files | .txt .nfo .sfv .md5 | .txt .nfo .sfv .md5 (CP437 .nfo art is decoded) |

Archives are identified by their content, so a `.cbr` that is really a ZIP (common) opens fine.
RAR and 7z pages are extracted in the background to a private temp folder that is deleted on close.

## Build

Needs Rust 1.88+ and a C++ compiler (the RAR decoder is bundled C++ source).

```sh
cargo build --release
./target/release/cdisplay-rs ~/Comics/SomeComic.cbz
```

Optional: `cargo build --release --features avif` (needs `libdav1d-dev`).
For `.cba`/ACE archives install `unar` (`sudo apt install unar`).

A desktop entry is in `packaging/cdisplay-rs.desktop`.

## Usage

```
cdisplay-rs [OPTIONS] [FILE_OR_FOLDER]
  -p, --page N        start at page N
  -w, --windowed      start in a window instead of full screen
  -r, --resume        reopen the last book at the last page read
      --export N OUT  write page N (with your colour settings) to an image file and exit
```

Opening a single image opens its whole folder at that image, as the original did. Drag and drop works too.

## Keys (same as CDisplay)

| Key | Action |
|---|---|
| Page Down / Page Up | Next / previous page (right double-click also = next) |
| Space / Shift+Space | Scroll down, then across, then next page |
| Arrows | Scroll; Left/Right turn the page when there's nothing to scroll |
| Home / End | First / last page |
| L, Shift+L, Shift+K | Load files; load next / previous file in the folder |
| R | Resume reading where you left off |
| P / Shift+P | Go to page: thumbnails / slider |
| A | Slideshow |
| C | Configure |
| W / M / X or Esc | Windowed / minimise / exit |
| F1 / F10 / right-click | Help / menu bar / pop-up menu |
| Ctrl+F, Ctrl+Shift+F | Fit, fit if oversize |
| Ctrl+W, Ctrl+Shift+W | Fit width, fit width if oversize |
| Ctrl+H, Ctrl+Shift+H | Fit height, fit height if oversize |
| Ctrl+S | Set height (configured pixel height) |
| Ctrl+T | Two pages side by side (double-width pages are shown alone) |
| Ctrl+J | Japanese (right-to-left) mode |
| Ctrl+L / Ctrl+R | Rotate all pages left / right |
| Ctrl+B / Ctrl+G / Ctrl+Y | Colour balance / gamma / yellow reduction |
| Ctrl+I / Ctrl+Z / Ctrl+C | Image information / magnifying glass / small cursor |

## Settings

Settings are stored in `~/.config/cdisplay-rs/config.toml` (the original used
`HKCU\Software\CDisplay`). Every option from the original's Configure dialog is there:
the sizing modes, colour balance with "discards per 1000", gamma (0.01–7.00),
yellow reduction, edge/internal yellow reduction, background (Auto/Black/White/Gray),
page buffer, Japanese mode, two-page stepping, and mouse actions.

## How the port was made

The original is a Borland C++Builder 5 program. Its form resources (DFM) were decoded
to recover every menu, shortcut, option and tooltip, and its x86 code was disassembled
to recover the settings defaults and the image filters:

* **Colour balance** (`TPageBMP::Balance`): a histogram of R+G+B (0–765); discards
  *N*/1000 of the pixels from the dark end (while below 64) and bright end (while above 196);
  skips the page if the remaining range/6 < 64; then applies one lookup table to all channels:
  `lut[i] = clamp((i − mid)·127 / range + 127)`.
* **Gamma** (`GammaCorrect`): `lut[i] = trunc(256 · (i/256)^(1/g))`, g = slider/100.
* **Yellow reduction** (`ReduceYellowing`): for pixels with R > 235, G > 220, B > 127,
  green and blue are pushed through ramps that add 3 per level above the threshold, capped at
  +10 (green) / +20 (blue). Pages whose interior is already more than 2% pure white (all channels > 250)
  are skipped.
* **Edge yellow reduction** defaults: thresholds 200/150/120 → colour (255, 240, 230). The edge
  flood fill is a re-implementation from its description.

These are reproduced in `src/filters.rs`, including the integer rounding, and are covered by unit tests.
No code or assets from the original were copied.

## Differences

* Smooth, mipmapped GPU scaling (toggle in Configure). The original scaled pixels on the CPU.
* Very large pages are reduced only to the GPU's texture limit.
* GIF and animated WebP show their first frame, as GIFs did in the original.
* The built-in "Load Files" browser stands in for the original's custom open dialog; the
  desktop's own dialog can be selected instead.
* Not ported: the PAR/SFV checksum verification, the "organise directories" favourites manager,
  and per-button treble-click mouse mappings.

## Layout

```
src/main.rs     command line, window setup, --export
src/app.rs      viewer: drawing, keys, mouse, menus, dialogs
src/source.rs   folders and archives (zip, rar, 7z, tar, external)
src/decode.rs   image decoding (image crate + jxl-oxide)
src/loader.rs   background decode/filter/rotate workers (the page buffer)
src/filters.rs  colour balance, gamma, yellow reduction (ported)
src/layout.rs   fit modes, rotation, two-page spreads
src/config.rs   settings file
```

`cargo test` runs the unit tests. Set `CDISPLAY_RAR_SAMPLE=/path/to/file.rar` (a RAR
containing a .txt) to also exercise the RAR extractor.

## Credits

CDisplay was created by David Ayton. This port is offered in his memory.
