//! Live reload via inotify. The parent directory is watched, not the file,
//! because many editors save by renaming a new file over the old one.

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Watcher {
    fd: i32,
    wd: i32,
    name: Arc<Mutex<OsString>>,
}

impl Watcher {
    pub fn new(on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
        // SAFETY: inotify_init1 takes no pointers.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let name = Arc::new(Mutex::new(OsString::new()));
        let thread_name = name.clone();
        std::thread::Builder::new()
            .name("watch".into())
            .stack_size(128 * 1024)
            .spawn(move || read_loop(fd, &thread_name, on_change))
            .ok()?;
        Some(Watcher { fd, wd: -1, name })
    }

    /// Switches the watch to `path`, or to nothing.
    pub fn set(&mut self, path: Option<&Path>) {
        if self.wd >= 0 {
            // SAFETY: fd and wd came from inotify calls and are plain integers.
            unsafe { libc::inotify_rm_watch(self.fd, self.wd) };
            self.wd = -1;
        }
        let Some(path) = path else { return };
        let (Some(dir), Some(file)) = (path.parent(), path.file_name()) else { return };
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        if let Ok(mut name) = self.name.lock() {
            *name = file.to_os_string();
        }
        let Ok(dir) = CString::new(dir.as_os_str().as_bytes()) else { return };
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE;
        // SAFETY: dir is a valid NUL-terminated string for the call's duration.
        self.wd = unsafe { libc::inotify_add_watch(self.fd, dir.as_ptr(), mask) };
    }
}

fn read_loop(fd: i32, name: &Mutex<OsString>, on_change: impl Fn()) {
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: buf is valid for writes of its full length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return;
        }
        let wanted = name.lock().map(|n| n.clone().into_vec()).unwrap_or_default();
        if events_name(&buf[..n as usize], &wanted) {
            // Let a burst of writes settle so a half-written file isn't loaded.
            std::thread::sleep(Duration::from_millis(40));
            on_change();
        }
    }
}

/// Reports whether a buffer of inotify events mentions the file `wanted`.
fn events_name(mut events: &[u8], wanted: &[u8]) -> bool {
    const HEADER: usize = 16;
    while events.len() >= HEADER {
        let len = u32::from_ne_bytes([events[12], events[13], events[14], events[15]]) as usize;
        let Some(raw) = events.get(HEADER..HEADER + len) else { return false };
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if OsStr::from_bytes(&raw[..end]).as_bytes() == wanted {
            return true;
        }
        events = &events[HEADER + len..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn parses_event_names() {
        let mut buf = vec![0u8; 12];
        buf.extend_from_slice(&8u32.to_ne_bytes());
        buf.extend_from_slice(b"a.md\0\0\0\0");
        assert!(events_name(&buf, b"a.md"));
        assert!(!events_name(&buf, b"b.md"));
    }

    #[test]
    fn notices_write_and_atomic_rename() {
        let dir = std::env::temp_dir().join(format!("markdonk-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.md");
        std::fs::write(&file, "one").unwrap();
        let (tx, rx) = channel();
        let mut watcher = Watcher::new(move || tx.send(()).unwrap()).unwrap();
        watcher.set(Some(&file));
        let timeout = Duration::from_secs(3);

        std::fs::write(&file, "two").unwrap();
        rx.recv_timeout(timeout).unwrap();
        while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}

        let tmp = dir.join("doc.md.tmp");
        std::fs::write(&tmp, "three").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        rx.recv_timeout(timeout).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
