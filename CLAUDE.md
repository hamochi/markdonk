# markdonk

A memory-efficient native Markdown viewer in Rust: one binary, fonts embedded, drawn on
the CPU with no webview or GPU context. Linux only for now; macOS was explicitly dropped.
`README.md` covers usage, keys and limits.

Memory use is the point of the project. Weigh any new dependency, cache or buffer against it.

## Commands

```sh
cargo test
cargo build --release     # about 3 minutes (fat LTO)
target/release/markdonk testdata/sample.md

# Paint one frame to a PNG with no window. Options must follow --render-png.
target/release/markdonk testdata/sample.md --render-png target/shots/a.png \
  --render-size 2000x1600 --render-scale 2 [--render-scroll PX|end] [--render-toc] [--render-search Q]

# Frame times at 3840x2320; needs a large Markdown file
MARKDONK_BENCH=big.md cargo test --release frame_times -- --ignored --nocapture
```

`target/shots/` is a convenient place for renders and generated test files, since `target/`
is ignored.

## Desktop integration

`assets/markdonk.desktop` and the install steps in `README.md` make it the default for
`text/markdown`. The installed binary is a copy, so rebuilding doesn't update it.

Without the `mimetype` command (from `perl-file-mimeinfo` on Arch), `xdg-open` falls back to
`file`, which reports `.md` as `text/plain`. `gio open` detects `text/markdown` correctly.

`softbuffer` always outputs opaque `Xrgb8888`, so a compositor opacity rule also fades the text,
and compositor effects that rely on a client's transparent pixels can't show through.

## Layout

| File | Role |
| --- | --- |
| `src/main.rs` | CLI, `winit` event loop, key bindings, `softbuffer` surface, `--render-png` |
| `src/doc.rs` | Markdown to a flat `Vec<Block>`, plus links, tables, TOC and anchors |
| `src/view.rs` | Lazy layout, scrolling, hit testing, selection, search, navigation |
| `src/view/edit.rs` | Inline editing: caret, undo, saving, disk conflicts |
| `src/draw.rs` | Themes and painting a frame |
| `src/canvas.rs` | Shape and blit primitives on the window's `0x00RRGGBB` buffer |
| `src/fonts.rs` | Embedded fonts, lazy system fonts, bounded glyph cache |
| `src/highlight.rs` | Table-driven syntax highlighter |
| `src/images.rs` | Worker-thread decoding, bounded pixel cache |
| `src/watch.rs` | inotify live reload |

## Design decisions to keep

- **Lazy layout:** only blocks near the viewport are shaped; the rest hold estimated heights.
  When a block above the viewport gets measured, `scroll` and `target` shift by the same
  delta, so visible content doesn't jump.
- **Flat blocks:** nesting becomes `level`, `quotes` and `marker` on each block, so any block
  can be laid out alone. Long code blocks are split into 64-line chunks and each table row is
  its own block, for the same reason.
- **Lazy highlighting:** `scan` runs at shape time. Parsing only records the lexer state at
  each chunk boundary.
- **Colours at draw time:** style and link index are packed into glyph metadata
  (`pack_meta`), so switching theme needs no re-layout.
- **No `tiny-skia`:** drawing directly into the window buffer avoids a second full-window
  buffer. `canvas.rs` exists for this reason.
- **Bounded caches:** glyphs are capped at 8 MB and decoded images at 64 MB.
- **System fonts:** scanned only when a document needs a character neither embedded font has.
  Text they already cover must not trigger the scan; a test checks this.
- **Dependencies:** built with `opt-level = "s"`, while this crate stays at `-O3` for painting.

## Editing

- The block under the caret becomes one `Kind::Source` block showing raw Markdown (`doc::activate`).
  Blocks record their top-level source range in `src`, so a list or table opens whole.
- Keystrokes only reshape that block. The whole document is re-parsed when the caret leaves it.
- The source text exists only in `View::edit`, so view mode holds no copy of it.
- Never lose work: saves are atomic and follow symlinks, `Esc` won't leave with unsaved changes,
  and a file changed on disk needs a second `Ctrl+S` to overwrite.

## Safety rules

- Opening a document must never touch the network. Remote images render as labelled links.
- `open_link` only passes `http`, `https` and `mailto` to `xdg-open`. Other schemes and local
  non-Markdown files are refused, because documents are untrusted input.
- Parsing and layout must not panic on malformed input. Text offsets are byte offsets, so
  respect char boundaries, including when slicing around non-ASCII text.

## Scrolling

- Touchpad deltas are multiplied by `TOUCHPAD_GAIN` (2.5) to undo Omarchy's
  `scroll_factor = 0.4`. `--scroll-speed` multiplies wheel and touchpad input.
- Momentum starts from `touch_ended`, driven by `winit`'s `TouchPhase::Ended`.
- Don't reset `last_tick` while an animation is running. That starves the next frame's step
  and made scrolling feel slow; a regression test covers it.

## Testing in the real window

Machine-specific steps, if any, are in `CLAUDE.local.md`, which isn't committed.

- A user may have their own markdonk windows open, so stop test windows by PID, never with
  `pkill -x markdonk`.
- Don't test clipboard copy or mouse actions without asking, since they would overwrite the
  user's clipboard or move their cursor.
- Check memory in `/proc/<pid>/status`: `RssShmem` is the window buffers, and `RssAnon` plus
  `RssFile` is the app itself. A second buffer appears after the second frame, so read it
  after interacting with the window.

## Measured baseline

In a 3840x2320 window, after the first version was built. Re-measure if a change could affect these.

| Measure | Result |
| --- | --- |
| Binary size | 6.1 MB (target under 6 MB, slightly over) |
| `testdata/sample.md`, excluding window buffers | 15 MB |
| `testdata/sample.md`, total with two buffers | 87 MB |
| 10 MB file, total with one buffer | 77 MB |
| First paint, sample document | About 30 ms |
| Paint while scrolling the 10 MB file | About 9 ms median in the real window |

## Not yet verified

- Clipboard copy, X11, and link clicks, drag selection and back navigation in a real window.
  Their logic has unit tests, but no one has confirmed them by hand.
- In the editor: mouse placement, paste and cut, and typing dead keys such as `` ` `` and `~`.
- Momentum settings (`FLING_FRICTION`, `TOUCHPAD_GAIN`) are tuned by judgement, not measured.

## Embedded fonts

`assets/fonts/` holds subsets of Inter 4.1 and JetBrains Mono 2.304 under the SIL OFL, with
their licenses alongside. `--licenses` prints them. They were made with `pyftsubset`, using:

```
--unicodes="U+0020-007E,U+00A0-024F,U+0370-03FF,U+0400-04FF,U+2000-206F,U+20A0-20BF,U+2100-214F,U+2190-21FF,U+2200-22FF,U+2300-23FF,U+2500-259F,U+25A0-25FF,U+2600-26FF,U+2700-27BF,U+FFFD"
--no-hinting --desubroutinize
--layout-features='kern,liga,ccmp,locl,mark,mkmk'    # Inter
--layout-features='kern,ccmp,locl,mark,mkmk'         # JetBrains Mono, no code ligatures
```
