//! "Books": a folder or archive of page images, plus any info files (.txt/.nfo).
//!
//! Replaces CDisplay's UnzDll.dll (ZIP), UnRar.dll (RAR) and UNACEV2.DLL (ACE)
//! with Rust crates, and adds 7-Zip. ACE (and anything the built-in readers
//! can't handle) falls back to the external `unar` or `7z` tools if installed.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

pub const IMAGE_EXTS: &[&str] = &[
    "jpg",
    "jpeg",
    "jpe",
    "jfif",
    "png",
    "gif",
    "bmp",
    "webp",
    "tif",
    "tiff",
    "tga",
    "ico",
    "pnm",
    "pbm",
    "pgm",
    "ppm",
    "qoi",
    "jxl",
    #[cfg(feature = "avif")]
    "avif",
];
pub const INFO_EXTS: &[&str] = &["txt", "nfo", "sfv", "md5"];
pub const ARCHIVE_EXTS: &[&str] = &[
    "zip", "cbz", "rar", "cbr", "7z", "cb7", "tar", "cbt", "ace", "cba",
];

fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
pub fn is_image_name(name: &str) -> bool {
    IMAGE_EXTS.contains(&ext_of(name).as_str())
}
pub fn is_info_name(name: &str) -> bool {
    INFO_EXTS.contains(&ext_of(name).as_str())
}
pub fn is_archive_name(name: &str) -> bool {
    ARCHIVE_EXTS.contains(&ext_of(name).as_str())
}
fn is_junk(name: &str) -> bool {
    name.split(['/', '\\'])
        .any(|c| c.starts_with('.') || c == "__MACOSX" || c.eq_ignore_ascii_case("thumbs.db"))
}

/// Natural ("human") ordering, case-insensitive: page2 < page10.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    natord::compare_ignore_case(a, b)
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Path inside the book, used for display and sorting.
    pub name: String,
    key: usize,
}

trait Backend: Send + Sync {
    fn read(&self, key: usize) -> Result<Vec<u8>>;
}

pub struct Book {
    /// The file or folder that was opened.
    pub path: PathBuf,
    pub title: String,
    pub kind: &'static str,
    pub pages: Vec<Entry>,
    pub info: Vec<Entry>,
    backend: Arc<dyn Backend>,
}

impl Book {
    pub fn read_page(&self, i: usize) -> Result<Vec<u8>> {
        let e = self
            .pages
            .get(i)
            .ok_or_else(|| anyhow!("page {i} out of range"))?;
        self.backend
            .read(e.key)
            .with_context(|| format!("reading {}", e.name))
    }
    pub fn read_info(&self, i: usize) -> Result<String> {
        let e = &self.info[i];
        let bytes = self.backend.read(e.key)?;
        Ok(String::from_utf8(bytes.clone()).unwrap_or_else(|_| cp437_to_string(&bytes)))
    }

    /// Open a folder, archive, or single image. For a single image the
    /// containing folder is opened and the returned index points at that image
    /// (as CDisplay did).
    pub fn open(path: &Path) -> Result<(Book, usize)> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if path.is_dir() {
            return Ok((open_folder(&path)?, 0));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let magic = read_magic(&path)?;
        let book = match sniff(&magic, &name) {
            Kind::Zip => open_zip(&path),
            Kind::Rar => open_rar(&path).or_else(|e| {
                log::warn!("built-in RAR failed ({e:#}), trying external tool");
                open_external(&path)
            }),
            Kind::SevenZ => open_7z(&path),
            Kind::Tar => open_tar(&path),
            Kind::Ace => open_external(&path),
            Kind::Image => {
                let dir = path.parent().ok_or_else(|| anyhow!("no parent folder"))?;
                let book = open_folder(dir)?;
                let idx = book.pages.iter().position(|e| e.name == name).unwrap_or(0);
                return Ok((book, idx));
            }
            Kind::Unknown => Err(anyhow!("{name}: not a supported image or archive")),
        }?;
        if book.pages.is_empty() {
            bail!("{} contains no supported images", book.title);
        }
        Ok((book, 0))
    }
}

#[derive(Debug, PartialEq)]
enum Kind {
    Zip,
    Rar,
    SevenZ,
    Tar,
    Ace,
    Image,
    Unknown,
}

fn read_magic(path: &Path) -> Result<Vec<u8>> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; 512];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// Content sniffing first (many .cbr files are really ZIPs and vice versa), then extension.
fn sniff(m: &[u8], name: &str) -> Kind {
    if m.starts_with(b"PK\x03\x04") || m.starts_with(b"PK\x05\x06") {
        Kind::Zip
    } else if m.starts_with(b"Rar!\x1a\x07") {
        Kind::Rar
    } else if m.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Kind::SevenZ
    } else if m.len() >= 14 && &m[7..14] == b"**ACE**" {
        Kind::Ace
    } else if m.len() >= 262 && &m[257..262] == b"ustar" {
        Kind::Tar
    } else if is_image_name(name) {
        Kind::Image
    } else {
        match ext_of(name).as_str() {
            "tar" | "cbt" => Kind::Tar,
            _ => Kind::Unknown,
        }
    }
}

/// Sort pages naturally and split images from info files.
fn build_entries(names: Vec<(String, usize)>) -> (Vec<Entry>, Vec<Entry>) {
    let mut pages = Vec::new();
    let mut info = Vec::new();
    for (name, key) in names {
        if is_junk(&name) {
            continue;
        }
        if is_image_name(&name) {
            pages.push(Entry { name, key });
        } else if is_info_name(&name) {
            info.push(Entry { name, key });
        }
    }
    pages.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    info.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    (pages, info)
}

fn title_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------- folder ----------

struct FolderBackend {
    files: Vec<PathBuf>,
}
impl Backend for FolderBackend {
    fn read(&self, key: usize) -> Result<Vec<u8>> {
        Ok(std::fs::read(&self.files[key])?)
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                walk(&p, depth + 1, out);
            }
        } else {
            out.push(p);
        }
    }
}

fn open_folder(dir: &Path) -> Result<Book> {
    let mut files = Vec::new();
    // Only descend into sub-folders when the folder itself has no images, so
    // opening a library folder doesn't swallow every comic below it.
    let top: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading folder {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    if top.iter().any(|p| is_image_name(&p.to_string_lossy())) {
        files = top;
    } else {
        walk(dir, 0, &mut files);
    }
    let names = files
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                p.strip_prefix(dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/"),
                i,
            )
        })
        .collect();
    let (pages, info) = build_entries(names);
    Ok(Book {
        path: dir.to_path_buf(),
        title: title_of(dir),
        kind: "Folder",
        pages,
        info,
        backend: Arc::new(FolderBackend { files }),
    })
}

// ---------- zip ----------

struct ZipBackend {
    archive: Mutex<zip::ZipArchive<File>>,
}
impl Backend for ZipBackend {
    fn read(&self, key: usize) -> Result<Vec<u8>> {
        let mut a = self.archive.lock().unwrap();
        let mut f = a.by_index(key)?;
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

fn open_zip(path: &Path) -> Result<Book> {
    let mut a = zip::ZipArchive::new(File::open(path)?).context("reading ZIP directory")?;
    let mut names = Vec::new();
    for i in 0..a.len() {
        let f = a.by_index_raw(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f
            .name()
            .map(|n| n.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(f.name_raw()).into_owned());
        names.push((name, i));
    }
    let (pages, info) = build_entries(names);
    Ok(Book {
        path: path.to_path_buf(),
        title: title_of(path),
        kind: "ZIP",
        pages,
        info,
        backend: Arc::new(ZipBackend {
            archive: Mutex::new(a),
        }),
    })
}

// ---------- tar ----------

struct TarBackend {
    path: PathBuf,
    spans: Vec<(u64, u64)>,
}
impl Backend for TarBackend {
    fn read(&self, key: usize) -> Result<Vec<u8>> {
        let (off, len) = self.spans[key];
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn open_tar(path: &Path) -> Result<Book> {
    let mut ar = tar::Archive::new(File::open(path)?);
    let mut spans = Vec::new();
    let mut names = Vec::new();
    for e in ar.entries()? {
        let e = e?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let name = e.path()?.to_string_lossy().into_owned();
        names.push((name, spans.len()));
        spans.push((e.raw_file_position(), e.size()));
    }
    let (pages, info) = build_entries(names);
    Ok(Book {
        path: path.to_path_buf(),
        title: title_of(path),
        kind: "TAR",
        pages,
        info,
        backend: Arc::new(TarBackend {
            path: path.to_path_buf(),
            spans,
        }),
    })
}

// ---------- background-extracted archives (RAR, 7z) ----------

enum Slot {
    Pending,
    Ready(PathBuf),
    Failed(String),
}

/// RAR and solid 7z archives are fastest to read front-to-back, so a
/// background thread extracts wanted entries to a private temp folder
/// while pages are being displayed; readers wait only for the page they need.
struct Extracted {
    _dir: tempfile::TempDir,
    slots: Mutex<Vec<Slot>>,
    cv: Condvar,
}
impl Extracted {
    fn new(n: usize) -> Result<Arc<Self>> {
        let dir = tempfile::Builder::new().prefix("cdisplay-").tempdir()?;
        Ok(Arc::new(Self {
            _dir: dir,
            slots: Mutex::new((0..n).map(|_| Slot::Pending).collect()),
            cv: Condvar::new(),
        }))
    }
    fn dir(&self) -> &Path {
        self._dir.path()
    }
    fn store(&self, key: usize, data: &[u8]) {
        let p = self.dir().join(format!("{key}.bin"));
        let slot = match std::fs::write(&p, data) {
            Ok(()) => Slot::Ready(p),
            Err(e) => Slot::Failed(e.to_string()),
        };
        self.slots.lock().unwrap()[key] = slot;
        self.cv.notify_all();
    }
    fn finish(&self, err: Option<String>) {
        let mut s = self.slots.lock().unwrap();
        for slot in s.iter_mut() {
            if matches!(slot, Slot::Pending) {
                *slot = Slot::Failed(
                    err.clone()
                        .unwrap_or_else(|| "entry not found in archive".into()),
                );
            }
        }
        self.cv.notify_all();
    }
}
impl Backend for Extracted {
    fn read(&self, key: usize) -> Result<Vec<u8>> {
        let mut s = self.slots.lock().unwrap();
        loop {
            match &s[key] {
                Slot::Ready(p) => return Ok(std::fs::read(p)?),
                Slot::Failed(e) => bail!("{e}"),
                Slot::Pending => s = self.cv.wait(s).unwrap(),
            }
        }
    }
}

fn open_rar(path: &Path) -> Result<Book> {
    let mut names = Vec::new();
    for e in unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|e| anyhow!("{e}"))?
    {
        let e = e.map_err(|e| anyhow!("{e}"))?;
        if e.is_file() {
            names.push(e.filename.to_string_lossy().replace('\\', "/"));
        }
    }
    let wanted: HashMap<String, usize> = names
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, n)| (n, i))
        .collect();
    let ex = Extracted::new(names.len())?;
    let (pages, info) = build_entries(names.into_iter().enumerate().map(|(i, n)| (n, i)).collect());
    let keep: std::collections::HashSet<usize> =
        pages.iter().chain(info.iter()).map(|e| e.key).collect();
    let ex2 = ex.clone();
    let p = path.to_path_buf();
    std::thread::Builder::new()
        .name("unrar".into())
        .spawn(move || {
            let run = || -> Result<()> {
                let mut a = unrar::Archive::new(&p)
                    .open_for_processing()
                    .map_err(|e| anyhow!("{e}"))?;
                while let Some(h) = a.read_header().map_err(|e| anyhow!("{e}"))? {
                    let name = h.entry().filename.to_string_lossy().replace('\\', "/");
                    match wanted.get(&name) {
                        Some(&k) if h.entry().is_file() && keep.contains(&k) => {
                            let (data, next) = h.read().map_err(|e| anyhow!("{e}"))?;
                            ex2.store(k, &data);
                            a = next;
                        }
                        _ => a = h.skip().map_err(|e| anyhow!("{e}"))?,
                    }
                }
                Ok(())
            };
            let r = run();
            ex2.finish(r.err().map(|e| format!("{e:#}")));
        })?;
    Ok(Book {
        path: path.to_path_buf(),
        title: title_of(path),
        kind: "RAR",
        pages,
        info,
        backend: ex,
    })
}

fn open_7z(path: &Path) -> Result<Book> {
    let archive = sevenz_rust2::Archive::open(path).map_err(|e| anyhow!("7z: {e}"))?;
    let names: Vec<String> = archive
        .files
        .iter()
        .filter(|f| !f.is_directory && f.has_stream)
        .map(|f| f.name.replace('\\', "/"))
        .collect();
    let wanted: HashMap<String, usize> = names
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, n)| (n, i))
        .collect();
    let ex = Extracted::new(names.len())?;
    let (pages, info) = build_entries(names.into_iter().enumerate().map(|(i, n)| (n, i)).collect());
    let ex2 = ex.clone();
    let p = path.to_path_buf();
    std::thread::Builder::new()
        .name("7z".into())
        .spawn(move || {
            let run = || -> Result<()> {
                let mut r = sevenz_rust2::ArchiveReader::open(&p, sevenz_rust2::Password::empty())
                    .map_err(|e| anyhow!("{e}"))?;
                r.for_each_entries(|entry, rd| {
                    let name = entry.name.replace('\\', "/");
                    let mut buf = Vec::new();
                    rd.read_to_end(&mut buf)?;
                    if let Some(&k) = wanted.get(&name) {
                        ex2.store(k, &buf);
                    }
                    Ok(true)
                })
                .map_err(|e| anyhow!("{e}"))?;
                Ok(())
            };
            let r = run();
            ex2.finish(r.err().map(|e| format!("{e:#}")));
        })?;
    Ok(Book {
        path: path.to_path_buf(),
        title: title_of(path),
        kind: "7-Zip",
        pages,
        info,
        backend: ex,
    })
}

// ---------- external tools (ACE and fallbacks) ----------

struct TempFolder {
    _dir: tempfile::TempDir,
    inner: FolderBackend,
}
impl Backend for TempFolder {
    fn read(&self, key: usize) -> Result<Vec<u8>> {
        self.inner.read(key)
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(cmd))
            .find(|p| p.is_file())
    })
}

fn open_external(path: &Path) -> Result<Book> {
    let dir = tempfile::Builder::new().prefix("cdisplay-").tempdir()?;
    let out = dir.path();
    let status = if let Some(unar) = which("unar") {
        std::process::Command::new(unar)
            .arg("-q")
            .arg("-f")
            .arg("-D")
            .arg("-o")
            .arg(out)
            .arg(path)
            .status()
    } else if let Some(sz) = which("7z").or_else(|| which("7zz")) {
        std::process::Command::new(sz)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", out.display()))
            .arg(path)
            .stdout(std::process::Stdio::null())
            .status()
    } else {
        bail!(
            "{}: this archive type needs the `unar` (or `7z`) program installed",
            title_of(path)
        );
    }?;
    if !status.success() {
        bail!("external extractor failed on {}", title_of(path));
    }
    let mut files = Vec::new();
    walk(out, 0, &mut files);
    let names = files
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                p.strip_prefix(out)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned(),
                i,
            )
        })
        .collect();
    let (pages, info) = build_entries(names);
    Ok(Book {
        path: path.to_path_buf(),
        title: title_of(path),
        kind: "Archive (external)",
        pages,
        info,
        backend: Arc::new(TempFolder {
            _dir: dir,
            inner: FolderBackend { files },
        }),
    })
}

// ---------- navigation between books ----------

/// Is `p` something CDisplay would open from a file list?
fn openable(p: &Path) -> bool {
    let n = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if n.starts_with('.') {
        return false;
    }
    if p.is_dir() {
        return std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .any(|e| is_image_name(&e.file_name().to_string_lossy()))
            })
            .unwrap_or(false);
    }
    is_archive_name(&n)
}

/// "Load Next Files" (Shift+L) / "Load Previous Files" (Shift+K): the
/// neighbouring archive or image folder in the same parent folder.
pub fn sibling(current: &Path, forward: bool) -> Option<PathBuf> {
    let parent = current.parent()?;
    let mut items: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p == current || openable(p))
        .collect();
    items.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    let i = items.iter().position(|p| p == current)?;
    if forward {
        items.get(i + 1).cloned()
    } else {
        i.checked_sub(1).and_then(|j| items.get(j).cloned())
    }
}

/// Decode CP437 (the DOS code page .nfo art is drawn in).
fn cp437_to_string(b: &[u8]) -> String {
    const HI: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    let hi: Vec<char> = HI.chars().collect();
    b.iter()
        .map(|&c| {
            if c < 128 {
                c as char
            } else {
                hi[(c - 128) as usize]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 6, image::Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["p10.jpg", "p2.jpg", "P1.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["P1.jpg", "p2.jpg", "p10.jpg"]);
    }

    #[test]
    fn zip_and_tar_books() {
        let dir = tempfile::tempdir().unwrap();
        let zp = dir.path().join("book.cbr"); // misnamed on purpose: sniffing must win
        {
            let mut z = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for n in ["10.png", "2.png", "__MACOSX/._2.png", "readme.nfo"] {
                z.start_file(n, o).unwrap();
                z.write_all(if n.ends_with("nfo") { b"hello" } else { &[] })
                    .unwrap();
                if n.ends_with("png") {
                    z.write_all(&png_bytes()).unwrap();
                }
            }
            z.finish().unwrap();
        }
        let (b, start) = Book::open(&zp).unwrap();
        assert_eq!(start, 0);
        assert_eq!(b.kind, "ZIP");
        assert_eq!(
            b.pages.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["2.png", "10.png"]
        );
        assert_eq!(b.read_info(0).unwrap(), "hello");
        assert_eq!(b.read_page(1).unwrap(), png_bytes());

        let tp = dir.path().join("book.cbt");
        {
            let mut t = tar::Builder::new(File::create(&tp).unwrap());
            for n in ["b.png", "a.png"] {
                let data = png_bytes();
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_cksum();
                t.append_data(&mut h, n, &data[..]).unwrap();
            }
            t.finish().unwrap();
        }
        let (b, _) = Book::open(&tp).unwrap();
        assert_eq!(b.pages[0].name, "a.png");
        assert_eq!(b.read_page(0).unwrap(), png_bytes());

        // single image opens its folder at that image
        std::fs::write(dir.path().join("x1.png"), png_bytes()).unwrap();
        std::fs::write(dir.path().join("x2.png"), png_bytes()).unwrap();
        let (b, i) = Book::open(&dir.path().join("x2.png")).unwrap();
        assert_eq!(b.pages[i].name, "x2.png");

        assert_eq!(
            sibling(&zp.canonicalize().unwrap(), true)
                .unwrap()
                .file_name()
                .unwrap(),
            "book.cbt"
        );
    }

    #[test]
    fn sevenz_book() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("01.png"), png_bytes()).unwrap();
        std::fs::write(src.join("02.png"), png_bytes()).unwrap();
        let out = dir.path().join("b.cb7");
        sevenz_rust2::compress_to_path(&src, &out).unwrap();
        let (b, _) = Book::open(&out).unwrap();
        assert_eq!(b.pages.len(), 2);
        assert_eq!(b.read_page(1).unwrap(), png_bytes());
    }

    /// Exercises the RAR backend (listing + background extraction) when
    /// CDISPLAY_RAR_SAMPLE points at a RAR containing a .txt file, e.g. the
    /// unrar crate's `data/unicode-entry.rar`.
    #[test]
    fn rar_background_extraction() {
        let Ok(data) = std::env::var("CDISPLAY_RAR_SAMPLE") else {
            return;
        };
        let b = open_rar(Path::new(&data)).unwrap();
        assert!(b.pages.is_empty());
        assert_eq!(b.info.len(), 1);
        let text = b.read_info(0).unwrap();
        println!("{} -> {text:?}", b.info[0].name);
    }
}
