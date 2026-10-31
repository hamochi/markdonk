mod canvas;
mod doc;
mod draw;
mod fonts;
mod highlight;
mod images;
mod view;
mod watch;

use softbuffer::{Context, Surface};
use std::io::{IsTerminal, Read};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use view::{Cursor, View};
use watch::Watcher;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{CursorIcon, Theme, Window, WindowId};

const USAGE: &str = "\
markdonk - a memory-efficient Markdown viewer

Usage: markdonk [options] <file.md>
       command | markdonk [options]

Options:
  --light         Start with the light theme
  --dark          Start with the dark theme
  --scroll-speed N  Multiply wheel and touchpad scrolling (default 1)
  --licenses      Print the licenses of the embedded fonts
  -h, --help      Show this help
  -V, --version   Show the version

Keys:
  j k, arrows     Scroll              /  Ctrl+F    Search
  Space, b        Page down, up       n  N         Next, previous match
  d u             Half page           t            Table of contents
  g G             Top, bottom         m            Light or dark theme
  Ctrl + - 0      Zoom                r            Reload
  Ctrl+C Ctrl+A   Copy, select all    Backspace    Back to previous file
  q               Quit                Esc          Close search, clear selection
";

const LICENSES: &str = concat!(
    "Inter\n\n",
    include_str!("../assets/fonts/LICENSE-Inter.txt"),
    "\n\nJetBrains Mono\n\n",
    include_str!("../assets/fonts/LICENSE-JetBrainsMono.txt"),
);

#[derive(Debug, Clone, Copy)]
enum Wake {
    Reload,
    Redraw,
}

struct Win {
    window: Arc<Window>,
    surface: Surface<Arc<Window>, Arc<Window>>,
    _context: Context<Arc<Window>>,
}

struct App {
    view: View,
    win: Option<Win>,
    watcher: Option<Watcher>,
    watched: Option<PathBuf>,
    mods: ModifiersState,
    pointer: (f32, f32),
    title: String,
    theme_forced: bool,
    cursor: Cursor,
}

impl App {
    fn sync_after_event(&mut self, event_loop: &ActiveEventLoop) {
        let Some(win) = &self.win else { return };
        if self.view.needs_redraw || self.view.animating() {
            win.window.request_redraw();
        }
        let title = self.view.title();
        if title != self.title {
            win.window.set_title(&title);
            self.title = title;
        }
        if self.view.cursor != self.cursor {
            self.cursor = self.view.cursor;
            win.window.set_cursor(match self.cursor {
                Cursor::Default => CursorIcon::Default,
                Cursor::Text => CursorIcon::Text,
                Cursor::Link => CursorIcon::Pointer,
            });
        }
        if self.view.path != self.watched {
            self.watched = self.view.path.clone();
            if let Some(w) = &mut self.watcher {
                w.set(self.watched.as_deref());
            }
        }
        event_loop.set_control_flow(match self.view.next_wake() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }

    fn key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let v = &mut self.view;
        let key = event.logical_key.as_ref();

        if v.search_editing() && !ctrl {
            match key {
                Key::Named(NamedKey::Escape) => v.close_search(),
                Key::Named(NamedKey::Enter) => {
                    v.search_step(!shift);
                    v.search_commit();
                }
                Key::Named(NamedKey::Backspace) => v.search_backspace(),
                _ => {
                    if let Some(text) = &event.text {
                        v.search_input(text);
                    }
                }
            }
            return;
        }

        match key {
            Key::Character(c) if ctrl => match c.to_ascii_lowercase().as_str() {
                "c" => v.copy(),
                "a" => v.select_all(),
                "f" => v.open_search(),
                "q" | "w" => event_loop.exit(),
                "=" | "+" => v.zoom_by(1.1),
                "-" | "_" => v.zoom_by(1.0 / 1.1),
                "0" => v.zoom_reset(),
                _ => {}
            },
            Key::Character("q") => event_loop.exit(),
            Key::Character("j") => v.scroll_lines(1.0),
            Key::Character("k") => v.scroll_lines(-1.0),
            Key::Character("d") => v.scroll_pages(0.5),
            Key::Character("u") => v.scroll_pages(-0.5),
            Key::Character("b") => v.scroll_pages(-1.0),
            Key::Character("g") => v.go_top(),
            Key::Character("G") => v.go_end(),
            Key::Character("/") => v.open_search(),
            Key::Character("n") => v.search_step(true),
            Key::Character("N") => v.search_step(false),
            Key::Character("t") => v.toggle_toc(),
            Key::Character("m") => {
                self.theme_forced = true;
                v.toggle_theme();
            }
            Key::Character("r") => v.reload(),
            Key::Named(NamedKey::ArrowDown) => v.scroll_lines(1.0),
            Key::Named(NamedKey::ArrowUp) => v.scroll_lines(-1.0),
            Key::Named(NamedKey::PageDown) => v.scroll_pages(1.0),
            Key::Named(NamedKey::PageUp) => v.scroll_pages(-1.0),
            Key::Named(NamedKey::Space) => v.scroll_pages(if shift { -1.0 } else { 1.0 }),
            Key::Named(NamedKey::Home) => v.go_top(),
            Key::Named(NamedKey::End) => v.go_end(),
            Key::Named(NamedKey::Backspace) => v.go_back(),
            Key::Named(NamedKey::ArrowLeft) if self.mods.alt_key() => v.go_back(),
            Key::Named(NamedKey::Escape) => {
                if v.search.is_some() {
                    v.close_search();
                } else {
                    v.clear_selection();
                }
            }
            _ => {}
        }
    }

    fn paint(&mut self) {
        let Some(win) = &mut self.win else { return };
        let size = win.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if win.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = win.surface.buffer_mut() else { return };
        self.view.draw(&mut buffer, w.get() as i32, h.get() as i32);
        win.window.pre_present_notify();
        if let Err(err) = buffer.present() {
            eprintln!("markdonk: present failed: {err}");
        }
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.view.title())
            .with_inner_size(LogicalSize::new(1000.0, 800.0))
            .with_name("markdonk", "markdonk");
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                eprintln!("markdonk: cannot open a window: {err}");
                event_loop.exit();
                return;
            }
        };
        let context = Context::new(window.clone());
        let surface = context.and_then(|ctx| Ok((Surface::new(&ctx, window.clone())?, ctx)));
        let (surface, context) = match surface {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("markdonk: cannot create a drawing surface: {err}");
                event_loop.exit();
                return;
            }
        };
        if !self.theme_forced {
            if let Some(theme) = window.theme() {
                self.view.dark = theme == Theme::Dark;
            }
        }
        let size = window.inner_size();
        self.view.resize(size.width as i32, size.height as i32, window.scale_factor() as f32);
        window.request_redraw();
        self.win = Some(Win { window, surface, _context: context });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(win) = &self.win {
                    let scale = win.window.scale_factor() as f32;
                    self.view.resize(size.width as i32, size.height as i32, scale);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(win) = &self.win {
                    let size = win.window.inner_size();
                    self.view.resize(size.width as i32, size.height as i32, scale_factor as f32);
                }
            }
            WindowEvent::RedrawRequested => self.paint(),
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => self.key(&event, event_loop),
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.pointer = (x as f32, y as f32);
                self.view.pointer_moved(x as f32, y as f32);
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                let (x, y) = self.pointer;
                match state {
                    ElementState::Pressed => self.view.pointer_down(x, y),
                    ElementState::Released => self.view.pointer_up(x, y),
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Back, .. } => {
                self.view.go_back();
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let (dy, pixels) = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-(y as f64), false),
                    MouseScrollDelta::PixelDelta(p) => (-p.y, true),
                };
                if self.mods.control_key() {
                    self.view.zoom_by(if dy < 0.0 { 1.1 } else { 1.0 / 1.1 });
                } else if dy != 0.0 {
                    self.view.wheel(dy, pixels, self.pointer.0);
                }
                if phase == TouchPhase::Ended {
                    self.view.touch_ended();
                }
            }
            _ => {}
        }
        self.sync_after_event(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Reload => self.view.reload(),
            Wake::Redraw => self.view.needs_redraw = true,
        }
        self.sync_after_event(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.view.tick_timers();
        self.sync_after_event(event_loop);
    }
}

struct Args {
    file: Option<PathBuf>,
    dark: Option<bool>,
    scroll_speed: f64,
    render: Option<Render>,
}

/// Debug aid: paint one frame to a PNG with no window.
struct Render {
    out: PathBuf,
    size: (i32, i32),
    scale: f32,
    scroll: Option<String>,
    toc: bool,
    search: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { file: None, dark: None, scroll_speed: 1.0, render: None };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy();
        let mut value = |name: &str| {
            it.next().map(|v| v.to_string_lossy().into_owned()).ok_or(format!("{name} needs a value"))
        };
        match text.as_ref() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("markdonk {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--licenses" => {
                print!("{LICENSES}");
                std::process::exit(0);
            }
            "--light" => args.dark = Some(false),
            "--dark" => args.dark = Some(true),
            "--scroll-speed" => {
                args.scroll_speed = value("--scroll-speed")?
                    .parse::<f64>()
                    .ok()
                    .filter(|n| *n > 0.0 && n.is_finite())
                    .ok_or("--scroll-speed takes a positive number")?;
            }
            "--render-png" => {
                args.render = Some(Render {
                    out: value("--render-png")?.into(),
                    size: (1000, 800),
                    scale: 1.0,
                    scroll: None,
                    toc: false,
                    search: None,
                });
            }
            "--render-size" | "--render-scale" | "--render-scroll" | "--render-search" => {
                let v = value(&text)?;
                let r = args.render.as_mut().ok_or("--render-png must come first")?;
                match text.as_ref() {
                    "--render-size" => {
                        let (w, h) = v.split_once('x').ok_or("--render-size takes WxH")?;
                        r.size = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
                    }
                    "--render-scale" => r.scale = v.parse().map_err(|_| "bad scale")?,
                    "--render-scroll" => r.scroll = Some(v),
                    _ => r.search = Some(v),
                }
            }
            "--render-toc" => args.render.as_mut().ok_or("--render-png must come first")?.toc = true,
            "-" => args.file = None,
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            _ if args.file.is_some() => return Err("only one file can be opened at a time".into()),
            _ => args.file = Some(PathBuf::from(&arg)),
        }
    }
    Ok(args)
}

fn render_png(view: &mut View, r: &Render) -> Result<(), String> {
    view.resize(r.size.0, r.size.1, r.scale);
    if r.toc {
        view.toggle_toc();
    }
    if let Some(query) = &r.search {
        view.open_search();
        view.search_input(query);
        view.search_commit();
    }
    match r.scroll.as_deref() {
        Some("end") => view.go_end(),
        Some(px) => view.scroll_pixels(px.parse().map_err(|_| "bad scroll")?),
        None => {}
    }
    let mut px = vec![0u32; (r.size.0 * r.size.1) as usize];
    view.draw(&mut px, r.size.0, r.size.1);
    view.images.wait_idle();
    view.draw(&mut px, r.size.0, r.size.1);
    let bytes: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    image::save_buffer(&r.out, &bytes, r.size.0 as u32, r.size.1 as u32, image::ColorType::Rgb8)
        .map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    // One heap arena: threads are few and short-lived work shouldn't pin memory.
    // SAFETY: called once at startup before any other threads exist.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 1) };

    let document = match &args.file {
        Some(path) => view::load_file(path).map_err(|e| format!("{}: {e}", path.display()))?,
        None => {
            if std::io::stdin().is_terminal() {
                return Err(format!("no file given\n\n{USAGE}"));
            }
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).map_err(|e| format!("stdin: {e}"))?;
            let base = std::env::current_dir().ok();
            doc::parse(&String::from_utf8_lossy(&bytes), base.as_deref())
        }
    };
    view::trim_heap();
    let dark = args.dark.unwrap_or(true);

    if let Some(render) = &args.render {
        let mut view = View::new(document, args.file.clone(), dark, Arc::new(|| {}));
        return render_png(&mut view, render);
    }

    let event_loop = EventLoop::<Wake>::with_user_event().build().map_err(|e| format!("cannot start: {e}"))?;
    let proxy: EventLoopProxy<Wake> = event_loop.create_proxy();
    let image_proxy = proxy.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = image_proxy.send_event(Wake::Redraw);
    });
    let watcher = args.file.as_ref().and_then(|_| {
        Watcher::new(move || {
            let _ = proxy.send_event(Wake::Reload);
        })
    });
    let mut app = App {
        view: View::new(document, args.file, dark, wake),
        win: None,
        watcher,
        watched: None,
        mods: ModifiersState::default(),
        pointer: (0.0, 0.0),
        title: String::new(),
        theme_forced: args.dark.is_some(),
        cursor: Cursor::Default,
    };
    app.view.scroll_speed = args.scroll_speed;
    event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"))
}

fn main() {
    if let Err(err) = run() {
        eprintln!("markdonk: {err}");
        std::process::exit(1);
    }
}
