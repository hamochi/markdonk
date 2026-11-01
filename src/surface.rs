//! Window pixel buffers. On Wayland these are our own shared-memory buffers, so
//! a window can have see-through pixels (`softbuffer` is always opaque there).
//! Other platforms go through `softbuffer` and stay opaque.

use softbuffer::{Context, Surface as SoftSurface};
use std::fs::File;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, FromRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::Window;

pub enum Surface {
    Wayland(ShmSurface),
    Soft {
        surface: SoftSurface<Arc<Window>, Arc<Window>>,
        _context: Context<Arc<Window>>,
    },
}

impl Surface {
    pub fn new(window: &Arc<Window>, transparent: bool) -> Result<Surface, String> {
        let display = window.display_handle().map_err(|e| e.to_string())?.as_raw();
        if let RawDisplayHandle::Wayland(_) = display {
            return ShmSurface::new(window, transparent).map(Surface::Wayland);
        }
        let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
        let surface = SoftSurface::new(&context, window.clone()).map_err(|e| e.to_string())?;
        Ok(Surface::Soft { surface, _context: context })
    }

    /// Lets `paint` fill a `w`×`h` buffer of premultiplied 0xAARRGGBB, then shows it.
    pub fn present(&mut self, window: &Window, w: u32, h: u32, paint: impl FnOnce(&mut [u32])) -> Result<(), String> {
        match self {
            Surface::Wayland(s) => s.present(window, w, h, paint),
            Surface::Soft { surface, .. } => {
                let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else { return Ok(()) };
                surface.resize(nw, nh).map_err(|e| e.to_string())?;
                let mut buffer = surface.buffer_mut().map_err(|e| e.to_string())?;
                paint(&mut buffer);
                // softbuffer wants the top byte clear.
                for p in buffer.iter_mut() {
                    *p &= 0x00ff_ffff;
                }
                window.pre_present_notify();
                buffer.present().map_err(|e| e.to_string())
            }
        }
    }
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for State {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, released: &Arc<AtomicBool>, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            released.store(true, Ordering::Relaxed);
        }
    }
}

struct ShmBuffer {
    buffer: wl_buffer::WlBuffer,
    ptr: *mut u32,
    w: u32,
    h: u32,
    released: Arc<AtomicBool>,
}

impl ShmBuffer {
    fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<State>, w: u32, h: u32, format: wl_shm::Format) -> Result<ShmBuffer, String> {
        let bytes = w as usize * h as usize * 4;
        // SAFETY: the name is NUL-terminated, and a successful call returns an fd we own.
        let file = unsafe {
            let fd = libc::memfd_create(c"markdonk".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            File::from_raw_fd(fd)
        };
        file.set_len(bytes as u64).map_err(|e| e.to_string())?;
        // SAFETY: mapping a fresh file of exactly `bytes` length, which nothing truncates.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), bytes, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&file), 0)
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let released = Arc::new(AtomicBool::new(true));
        let pool = shm.create_pool(file.as_fd(), bytes as i32, qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, w as i32 * 4, format, qh, released.clone());
        // The buffer keeps the memory alive; neither pool nor file are needed again.
        pool.destroy();
        Ok(ShmBuffer { buffer, ptr: ptr.cast(), w, h, released })
    }

    fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: ptr maps w*h*4 bytes for this struct's lifetime. The compositor
        // only reads it, and only between attach and release; callers wait for release.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.w as usize * self.h as usize) }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        // SAFETY: ptr and the length are exactly what mmap was given and returned.
        unsafe { libc::munmap(self.ptr.cast(), self.w as usize * self.h as usize * 4) };
    }
}

pub struct ShmSurface {
    buffers: [Option<ShmBuffer>; 2],
    front: usize,
    surface: wl_surface::WlSurface,
    shm: wl_shm::WlShm,
    format: wl_shm::Format,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    conn: Connection,
    // Owns the display and surface that the connection above borrows.
    _window: Arc<Window>,
}

impl ShmSurface {
    fn new(window: &Arc<Window>, transparent: bool) -> Result<ShmSurface, String> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().map_err(|e| e.to_string())?.as_raw() else {
            return Err("not a Wayland display".into());
        };
        let RawWindowHandle::Wayland(handle) = window.window_handle().map_err(|e| e.to_string())?.as_raw() else {
            return Err("not a Wayland window".into());
        };
        // SAFETY: the pointers come from winit and stay valid while `_window` is held.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
        // SAFETY: as above; this really is a wl_surface.
        let id = unsafe { ObjectId::from_ptr(wl_surface::WlSurface::interface(), handle.surface.as_ptr().cast()) }
            .map_err(|e| e.to_string())?;
        let surface = wl_surface::WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;
        // Opaque windows skip blending in the compositor.
        let format = if transparent { wl_shm::Format::Argb8888 } else { wl_shm::Format::Xrgb8888 };
        Ok(ShmSurface { buffers: [None, None], front: 0, surface, shm, format, queue, qh, conn, _window: window.clone() })
    }

    fn present(&mut self, window: &Window, w: u32, h: u32, paint: impl FnOnce(&mut [u32])) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let back = 1 - self.front;
        self.queue.dispatch_pending(&mut State).map_err(|e| e.to_string())?;
        while self.buffers[back].as_ref().is_some_and(|b| !b.released.load(Ordering::Relaxed)) {
            self.queue.blocking_dispatch(&mut State).map_err(|e| e.to_string())?;
        }
        if !self.buffers[back].as_ref().is_some_and(|b| b.w == w && b.h == h) {
            self.buffers[back] = Some(ShmBuffer::new(&self.shm, &self.qh, w, h, self.format)?);
        }
        let Some(buffer) = &mut self.buffers[back] else { return Ok(()) };
        paint(buffer.pixels());
        buffer.released.store(false, Ordering::Relaxed);
        window.pre_present_notify();
        self.surface.attach(Some(&buffer.buffer), 0, 0);
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, w as i32, h as i32);
        } else {
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        self.surface.commit();
        self.front = back;
        self.conn.flush().map_err(|e| e.to_string())
    }
}
