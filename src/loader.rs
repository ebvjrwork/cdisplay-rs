//! Background page pipeline: read → decode → filters → rotate, on worker
//! threads, so page turns are instant (CDisplay's "Page Buffer").

use crate::config::ProcessingKey;
use crate::decode;
use crate::filters::{self, BalanceOutcome};
use crate::layout::Rotation;
use crate::source::Book;
use image::RgbaImage;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    Page,
    Thumb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Job {
    pub index: usize,
    pub kind: JobKind,
    pub rotation: Rotation,
    pub processing: ProcessingKey,
    pub max_side: u32,
}

pub struct Processed {
    pub image: RgbaImage,
    pub original_size: (u32, u32),
    pub format: String,
    pub file_bytes: usize,
    pub balance: Option<BalanceOutcome>,
    pub yellow_applied: Option<bool>,
    pub edge_colour: [u8; 3],
    pub millis: u128,
}

pub struct JobResult {
    pub job: Job,
    pub outcome: Result<Processed, String>,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    active: Mutex<Vec<Job>>,
    cv: Condvar,
    stop: AtomicBool,
}

pub struct Loader {
    shared: Arc<Shared>,
    pub results: crossbeam_channel::Receiver<JobResult>,
}

impl Loader {
    pub fn new(book: Arc<Book>, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            active: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let notify = Arc::new(notify);
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 4);
        for i in 0..n {
            let shared = shared.clone();
            let tx = tx.clone();
            let book = book.clone();
            let notify = notify.clone();
            std::thread::Builder::new()
                .name(format!("page-loader-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let mut q = shared.queue.lock().unwrap();
                            loop {
                                if shared.stop.load(Ordering::Relaxed) {
                                    return;
                                }
                                if let Some(j) = q.pop_front() {
                                    shared.active.lock().unwrap().push(j);
                                    break j;
                                }
                                q = shared.cv.wait(q).unwrap();
                            }
                        };
                        let outcome = run_job(&book, &job).map_err(|e| format!("{e:#}"));
                        // Publish the result before clearing `active`, so the UI never
                        // sees the job as neither running nor finished.
                        let sent = !shared.stop.load(Ordering::Relaxed)
                            && tx.send(JobResult { job, outcome }).is_ok();
                        {
                            let mut a = shared.active.lock().unwrap();
                            if let Some(i) = a.iter().position(|j| *j == job) {
                                a.swap_remove(i);
                            }
                        }
                        if !sent {
                            return;
                        }
                        notify();
                    }
                })
                .expect("spawn loader");
        }
        Self {
            shared,
            results: rx,
        }
    }

    /// Jobs currently being worked on.
    pub fn active(&self) -> Vec<Job> {
        self.shared.active.lock().unwrap().clone()
    }

    /// Replace pending work with `jobs` (in priority order). Jobs already being
    /// processed finish normally.
    pub fn set_queue(&self, jobs: Vec<Job>) {
        let mut q = self.shared.queue.lock().unwrap();
        q.clear();
        q.extend(jobs);
        self.shared.cv.notify_all();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.cv.notify_all();
    }
}

pub fn run_job(book: &Book, job: &Job) -> anyhow::Result<Processed> {
    let t0 = Instant::now();
    let bytes = book.read_page(job.index)?;
    let file_bytes = bytes.len();
    let decoded = decode::decode(&bytes)?;
    drop(bytes);
    let mut img = decoded.image;
    let original_size = img.dimensions();

    if job.kind == JobKind::Thumb {
        let h = job.max_side;
        let w =
            ((img.width() as f32 * h as f32 / img.height().max(1) as f32).round() as u32).max(1);
        img = image::imageops::thumbnail(&img, w, h);
        img = job.rotation.apply(&img);
        return Ok(Processed {
            image: img,
            original_size,
            format: decoded.format,
            file_bytes,
            balance: None,
            yellow_applied: None,
            edge_colour: [0; 3],
            millis: t0.elapsed().as_millis(),
        });
    }

    // Shrink only what the GPU can't hold; normal down-scaling happens on the GPU with mipmaps.
    let max = job.max_side.max(1024);
    if img.width() > max || img.height() > max {
        let s = max as f32 / img.width().max(img.height()) as f32;
        img = image::imageops::resize(
            &img,
            ((img.width() as f32 * s) as u32).max(1),
            ((img.height() as f32 * s) as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }

    let p = job.processing;
    if let Some(e) = p.edge {
        filters::reduce_edge_yellowing(&mut img, &e);
    }
    if let Some(e) = p.internal {
        filters::reduce_internal_yellowing(&mut img, &e);
    }
    let yellow_applied = p.yellow.map(|y| filters::reduce_yellowing(&mut img, &y));
    let balance = p.balance.map(|d| filters::colour_balance(&mut img, d));
    if let Some(g) = p.gamma {
        filters::gamma_correct(&mut img, g);
    }
    let edge_colour = filters::edge_colour(&img);
    if job.rotation != Rotation::Normal {
        img = job.rotation.apply(&img);
    }
    Ok(Processed {
        image: img,
        original_size,
        format: decoded.format,
        file_bytes,
        balance,
        yellow_applied,
        edge_colour,
        millis: t0.elapsed().as_millis(),
    })
}
