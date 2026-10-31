//! Decodes images on a worker thread, scaled to display size. Pixels are kept
//! only for blocks near the viewport and under a fixed byte budget.

use image::imageops::FilterType;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

const MAX_PIXELS: u64 = 100_000_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Box<[u32]>,
}

struct Request {
    generation: u64,
    block: u32,
    path: PathBuf,
    width: u32,
    height: u32,
}

struct Done {
    generation: u64,
    block: u32,
    pixels: Option<Pixels>,
}

pub struct Images {
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<(Sender<Request>, Receiver<Done>)>,
    generation: u64,
    pub ready: HashMap<u32, Pixels>,
    pending: HashSet<u32>,
    failed: HashSet<u32>,
    bytes: usize,
}

impl Images {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Images {
            wake,
            worker: None,
            generation: 0,
            ready: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            bytes: 0,
        }
    }

    /// Forgets everything; results still in flight are discarded on arrival.
    pub fn reset(&mut self) {
        self.generation += 1;
        self.ready = HashMap::new();
        self.pending.clear();
        self.failed.clear();
        self.bytes = 0;
    }

    pub fn request(&mut self, block: u32, path: &Path, width: u32, height: u32) {
        if self.failed.contains(&block) || self.pending.contains(&block) {
            return;
        }
        if let Some(p) = self.ready.get(&block) {
            if p.width == width && p.height == height {
                return;
            }
        }
        if self.worker.is_none() {
            self.worker = Some(spawn_worker(self.wake.clone()));
        }
        let (tx, _) = self.worker.as_ref().unwrap();
        let req = Request { generation: self.generation, block, path: path.to_path_buf(), width, height };
        if tx.send(req).is_ok() {
            self.pending.insert(block);
        }
    }

    /// Collects finished decodes. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let Some((_, rx)) = &self.worker else { return false };
        let mut changed = false;
        while let Ok(done) = rx.try_recv() {
            if done.generation != self.generation {
                continue;
            }
            self.pending.remove(&done.block);
            match done.pixels {
                Some(p) => {
                    self.bytes += p.data.len() * 4;
                    if let Some(old) = self.ready.insert(done.block, p) {
                        self.bytes -= old.data.len() * 4;
                    }
                }
                None => {
                    self.failed.insert(done.block);
                }
            }
            changed = true;
        }
        changed
    }

    /// Blocks until outstanding decodes finish; used by offscreen rendering.
    pub fn wait_idle(&mut self) {
        while !self.pending.is_empty() {
            self.poll();
            std::thread::yield_now();
        }
    }

    /// Drops pixels outside `keep`, then the blocks furthest from `center`
    /// until the budget holds.
    pub fn evict(&mut self, keep: std::ops::Range<u32>, center: u32) {
        let mut bytes = self.bytes;
        self.ready.retain(|block, p| {
            let retained = keep.contains(block);
            if !retained {
                bytes -= p.data.len() * 4;
            }
            retained
        });
        self.bytes = bytes;
        while self.bytes > MAX_BYTES && self.ready.len() > 1 {
            let far = *self.ready.keys().max_by_key(|b| b.abs_diff(center)).unwrap();
            if let Some(p) = self.ready.remove(&far) {
                self.bytes -= p.data.len() * 4;
            }
        }
    }

    pub fn is_failed(&self, block: u32) -> bool {
        self.failed.contains(&block)
    }
}

fn spawn_worker(wake: Arc<dyn Fn() + Send + Sync>) -> (Sender<Request>, Receiver<Done>) {
    let (req_tx, req_rx) = channel::<Request>();
    let (done_tx, done_rx) = channel::<Done>();
    let spawned = std::thread::Builder::new()
        .name("images".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            for req in req_rx {
                let pixels = decode(&req.path, req.width, req.height);
                let done = Done { generation: req.generation, block: req.block, pixels };
                if done_tx.send(done).is_err() {
                    return;
                }
                wake();
            }
        });
    if let Err(err) = spawned {
        eprintln!("markdonk: cannot start image thread: {err}");
    }
    (req_tx, done_rx)
}

pub fn decode(path: &Path, width: u32, height: u32) -> Option<Pixels> {
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let (natural_w, natural_h) = reader.into_dimensions().ok()?;
    if natural_w as u64 * natural_h as u64 > MAX_PIXELS {
        return None;
    }
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut decoded = reader.decode().ok()?;
    if decoded.width() != width || decoded.height() != height {
        decoded = decoded.resize_exact(width.max(1), height.max(1), FilterType::Triangle);
    }
    let rgba = decoded.into_rgba8();
    let data = rgba
        .pixels()
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect();
    Some(Pixels { width: rgba.width(), height: rgba.height(), data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_scales_and_converts_channels() {
        let dir = std::env::temp_dir().join(format!("markdonk-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        image::RgbaImage::from_pixel(4, 2, image::Rgba([10, 20, 30, 255])).save(&path).unwrap();
        let p = decode(&path, 2, 1).unwrap();
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(p.data[0], 0xff0a141e);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decode_rejects_non_images() {
        let dir = std::env::temp_dir().join(format!("markdonk-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(decode(&path, 2, 1).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
