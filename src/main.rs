mod canvas;
mod doc;
mod draw;
mod fonts;
mod highlight;
mod images;
mod pages;
mod surface;
mod theme;
mod view;
mod watch;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;
use surface::Surface;
use view::{Appearance, Cursor, View};
use watch::Watcher;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{CursorIcon, Theme as WindowTheme, Window, WindowId};

const USAGE: &str = "\
markdonk - a memory-efficient Markdown viewer

Usage: markdonk [options] <file.md>
       command | markdonk [options]

Options:
  --light         Start with the light theme
  --dark          Start with the dark theme
  --scroll-speed N  Multiply wheel and touchpad scrolling (default 1)
  --background-opacity N  See-through window background, 0 to 1 (default 1)
  --licenses      Print the licenses of the embedded fonts
  -h, --help      Show this help
  -V, --version   Show the version

Keys:
  j k, arrows     Scroll              /  Ctrl+F    Search
  Space, b        Page down, up       n  N         Next, previous match
  d u             Half page           t            Table of contents
  g G             Top, bottom         m            System, dark or light theme
  Ctrl + - 0      Zoom                r            Reload
  Ctrl+C Ctrl+A   Copy, select all    Backspace    Back to previous file
  q               Quit                Esc          Close search, clear selection
  e               Edit in place: Ctrl+S saves, Esc finishes, Ctrl+Z undoes
  o               Browse for a file
  Tab, Enter      Move between links, open the focused one
";

const LICENSES: &str = concat!(
    "Inter\n\n",
    include_str!("../assets/fonts/LICENSE-Inter.txt"),
    "\n\nJetBrains Mono\n\n",
    include_str!("../assets/fonts/LICENSE-JetBrainsMono.txt"),
);

#[derive(Debug)]
enum Wake {
    Reload,
    Redraw,
    Theme,
    FontFile,
    Font(String),
}

struct Win {
    window: Arc<Window>,
    surface: Surface,
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
    transparent: bool,
    proxy: EventLoopProxy<Wake>,
    // Held so their threads keep watching for Omarchy theme and font changes.
    _desktop_watchers: Vec<Watcher>,
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

        if v.editing() {
            self.edit_key(event, event_loop);
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
            Key::Character("e") => v.enter_edit(),
            Key::Character("o") => v.open_browser(),
            Key::Named(NamedKey::Tab) => v.focus_next_link(!shift),
            Key::Named(NamedKey::Enter) => v.activate_focused_link(),
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
                    v.clear_link_focus();
                }
            }
            _ => {}
        }
    }

    fn edit_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let v = &mut self.view;
        match event.logical_key.as_ref() {
            Key::Character(c) if ctrl => match c.to_ascii_lowercase().as_str() {
                "s" => v.edit_save(),
                "z" if shift => v.edit_redo(),
                "z" => v.edit_undo(),
                "y" => v.edit_redo(),
                "c" => v.copy(),
                "x" => v.edit_cut(),
                "v" => v.edit_paste(),
                "a" => v.edit_select_block(),
                "f" => v.open_search(),
                "q" if shift => v.edit_discard(),
                "q" | "w" => {
                    if v.request_quit() {
                        event_loop.exit();
                    }
                }
                "=" | "+" => v.zoom_by(1.1),
                "-" | "_" => v.zoom_by(1.0 / 1.1),
                "0" => v.zoom_reset(),
                _ => {}
            },
            Key::Named(NamedKey::Escape) => {
                if v.search.is_some() {
                    v.close_search();
                } else {
                    v.edit_escape();
                }
            }
            Key::Named(NamedKey::ArrowLeft) => v.caret_left(ctrl, shift),
            Key::Named(NamedKey::ArrowRight) => v.caret_right(ctrl, shift),
            Key::Named(NamedKey::ArrowUp) => v.caret_vertical(false, shift),
            Key::Named(NamedKey::ArrowDown) => v.caret_vertical(true, shift),
            Key::Named(NamedKey::Home) => v.caret_line_edge(false, shift),
            Key::Named(NamedKey::End) => v.caret_line_edge(true, shift),
            Key::Named(NamedKey::PageDown) => v.scroll_pages(1.0),
            Key::Named(NamedKey::PageUp) => v.scroll_pages(-1.0),
            Key::Named(NamedKey::Backspace) => v.edit_backspace(ctrl),
            Key::Named(NamedKey::Delete) => v.edit_delete(ctrl),
            Key::Named(NamedKey::Enter) => v.edit_insert("\n"),
            Key::Named(NamedKey::Tab) => v.edit_insert("    "),
            _ if !ctrl => {
                if let Some(text) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
                    v.edit_insert(text);
                }
            }
            _ => {}
        }
    }

    fn paint(&mut self) {
        let Some(win) = &mut self.win else { return };
        let size = win.window.inner_size();
        let view = &mut self.view;
        let (w, h) = (size.width, size.height);
        let painted = win.surface.present(&win.window, w, h, |px| view.draw(px, w as i32, h as i32));
        if let Err(err) = painted {
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
            .with_transparent(self.transparent)
            .with_name("markdonk", "markdonk");
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                eprintln!("markdonk: cannot open a window: {err}");
                event_loop.exit();
                return;
            }
        };
        let surface = match Surface::new(&window, self.transparent) {
            Ok(surface) => surface,
            Err(err) => {
                eprintln!("markdonk: cannot create a drawing surface: {err}");
                event_loop.exit();
                return;
            }
        };
        if !self.theme_forced {
            if let Some(theme) = window.theme() {
                self.view.dark = theme == WindowTheme::Dark;
            }
            self.view.set_appearance(Appearance::Follow);
        }
        let size = window.inner_size();
        self.view.resize(size.width as i32, size.height as i32, window.scale_factor() as f32);
        window.request_redraw();
        self.win = Some(Win { window, surface });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if self.view.request_quit() {
                    event_loop.exit();
                }
            }
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
            Wake::Theme => self.view.theme_changed(),
            Wake::FontFile => spawn_font_query(self.proxy.clone()),
            Wake::Font(family) => self.view.set_mono_font(&family),
        }
        self.sync_after_event(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.view.tick_timers();
        self.sync_after_event(event_loop);
    }
}

/// Asks fontconfig, which is what `omarchy font` uses, for the monospace font.
/// It runs on its own thread so a slow answer can't delay the first frame.
fn spawn_font_query(proxy: EventLoopProxy<Wake>) {
    std::thread::spawn(move || {
        let output = std::process::Command::new("fc-match").args(["monospace", "-f", "%{family}"]).output();
        let family = output.ok().filter(|o| o.status.success()).and_then(|o| String::from_utf8(o.stdout).ok());
        if let Some(family) = family.as_deref().and_then(|f| f.split(',').next()).map(str::trim).filter(|f| !f.is_empty()) {
            let _ = proxy.send_event(Wake::Font(family.to_string()));
        }
    });
}

fn watch_desktop(proxy: &EventLoopProxy<Wake>) -> Vec<Watcher> {
    let mut watchers = Vec::new();
    let mut add = |path: Option<PathBuf>, wake: fn() -> Wake| {
        let (Some(path), proxy) = (path, proxy.clone()) else { return };
        if let Some(mut watcher) = Watcher::new(move || {
            let _ = proxy.send_event(wake());
        }) {
            watcher.set(Some(&path));
            watchers.push(watcher);
        }
    };
    add(theme::omarchy_theme_marker(), || Wake::Theme);
    add(theme::fontconfig_file(), || Wake::FontFile);
    watchers
}

struct Args {
    file: Option<PathBuf>,
    dark: Option<bool>,
    scroll_speed: f64,
    opacity: Option<f32>,
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
    let mut args = Args { file: None, dark: None, scroll_speed: 1.0, opacity: None, render: None };
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
            "--background-opacity" => {
                let opacity = value("--background-opacity")?.parse::<f32>().ok();
                let opacity = opacity.filter(|n| n.is_finite() && (0.0..=1.0).contains(n));
                args.opacity = Some(opacity.ok_or("--background-opacity takes a number from 0 to 1")?);
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

    let mut start_page = false;
    let document = match &args.file {
        Some(path) => {
            let document = view::load_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
            pages::remember(path);
            document
        }
        None => {
            // Launchers give us an empty stdin; a terminal has nothing piped in.
            let mut bytes = Vec::new();
            if !std::io::stdin().is_terminal() {
                std::io::stdin().read_to_end(&mut bytes).map_err(|e| format!("stdin: {e}"))?;
            }
            start_page = bytes.is_empty();
            let base = std::env::current_dir().ok();
            doc::parse(&String::from_utf8_lossy(&bytes), base.as_deref())
        }
    };
    view::trim_heap();
    let dark = args.dark.unwrap_or(true);
    let opacity = args.opacity.or_else(theme::configured_opacity).unwrap_or(1.0);
    let background_alpha = (opacity * 255.0).round() as u32;
    let setup = |view: &mut View| {
        if start_page {
            view.show_start_page();
        }
        view.background_alpha = background_alpha;
        view.scroll_speed = args.scroll_speed;
        if args.dark.is_none() {
            view.set_appearance(Appearance::Follow);
        }
    };

    if let Some(render) = &args.render {
        let mut view = View::new(document, args.file.clone(), dark, Arc::new(|| {}));
        setup(&mut view);
        return render_png(&mut view, render);
    }

    let event_loop = EventLoop::<Wake>::with_user_event().build().map_err(|e| format!("cannot start: {e}"))?;
    let proxy: EventLoopProxy<Wake> = event_loop.create_proxy();
    let image_proxy = proxy.clone();
    let reload_proxy = proxy.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = image_proxy.send_event(Wake::Redraw);
    });
    let watcher = args.file.as_ref().and_then(|_| {
        Watcher::new(move || {
            let _ = reload_proxy.send_event(Wake::Reload);
        })
    });
    let mut view = View::new(document, args.file.clone(), dark, wake);
    setup(&mut view);
    spawn_font_query(proxy.clone());
    let mut app = App {
        view,
        win: None,
        watcher,
        watched: None,
        mods: ModifiersState::default(),
        pointer: (0.0, 0.0),
        title: String::new(),
        theme_forced: args.dark.is_some(),
        cursor: Cursor::Default,
        transparent: background_alpha < 255,
        _desktop_watchers: watch_desktop(&proxy),
        proxy,
    };
    event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"))
}

fn main() {
    if let Err(err) = run() {
        eprintln!("markdonk: {err}");
        std::process::exit(1);
    }
}
