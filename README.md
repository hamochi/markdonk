# markdonk

A memory-efficient native Markdown viewer and editor for Linux. It ships as one binary with
its fonts inside, and draws everything itself on the CPU, with no webview or GPU
context.

```sh
cargo build --release
target/release/markdonk README.md
some-command | target/release/markdonk
```

## Install

This installs for the current user and makes markdonk the default for Markdown files.
Rerun it after rebuilding to update the installed copy.

```sh
install -Dm755 target/release/markdonk ~/.local/bin/markdonk
install -Dm644 assets/markdonk.desktop ~/.local/share/applications/markdonk.desktop
install -Dm644 assets/markdonk.svg ~/.local/share/icons/hicolor/scalable/apps/markdonk.svg
update-desktop-database ~/.local/share/applications
xdg-mime default markdonk.desktop text/markdown text/x-markdown
```

`xdg-open` also needs the `mimetype` command to recognise `.md` files; on Arch it's in the
`perl-file-mimeinfo` package. Without it, `xdg-open` treats them as plain text.

## Features

- GitHub-flavored Markdown: tables, task lists, footnotes, alerts and strikethrough
- Syntax colours for common languages
- Local PNG, JPEG, WebP and GIF (first frame) images
- Search, table of contents, text selection and copy, zoom, light and dark themes
- Links between `.md` files with back navigation, and live reload on save

## Keys

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| `j` `k`, arrows | Scroll | `/`, `Ctrl+F` | Search |
| `Space`, `b` | Page down, up | `n`, `N` | Next, previous match |
| `d`, `u` | Half page | `t` | Table of contents |
| `g`, `G` | Top, bottom | `m` | System, dark or light theme |
| `o` | Browse for a file | `Tab`, `Enter` | Move between links, open one |
| `Ctrl` `+` `-` `0` | Zoom | `r` | Reload |
| `Ctrl+C`, `Ctrl+A` | Copy, select all | `Backspace` | Previous file |
| `q` | Quit | `Esc` | Close search, clear selection |

## Editing

Press `e` to edit in place. The block under the caret shows raw Markdown, and the rest of
the document stays rendered. Moving the caret to another block, or clicking one, renders
the block you left.

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| `Ctrl+S` | Save | `Esc` | Finish editing |
| `Ctrl+Z`, `Ctrl+Y` | Undo, redo | `Ctrl+Shift+Q` | Discard changes |
| `Ctrl+C`, `Ctrl+X`, `Ctrl+V` | Copy, cut, paste | `Ctrl+A` | Select the block |

Saves replace the file atomically. A list or table is one block, so it opens whole. If the
file changes on disk while you have unsaved edits, the next save warns before overwriting.

## Omarchy

markdonk fits in with no setup, and falls back to its own defaults elsewhere:

- **Theme:** colours come from the active Omarchy theme and update live when it changes.
  `--dark` and `--light` opt out, and `m` switches between system, dark and light.
- **Font:** code and the editing block use the system monospace font, from `omarchy font set`
  or any fontconfig setup.

A see-through background keeps text fully solid. Add this to `~/.config/markdonk/config`, or
pass `--background-opacity`:

```
background-opacity = 0.9
```

A Hyprland rule sets touchpad speed, the same way Omarchy tunes Ghostty:

```lua
o.window("markdonk", { scroll_touchpad = 1 })
```

## Scrolling

Touchpad scrolling coasts after a flick. Hyprland's touchpad `scroll_factor` applies, and
`--scroll-speed N` multiplies wheel and touchpad scrolling.

## Limits

- Remote images are shown as links. Opening a document makes no network requests.
- Raw HTML: only `<br>` and `<img>` have an effect. Other tags are dropped, and
  their text stays visible.
- No math or Mermaid diagrams, and no screen reader support.
- Links open only for `http`, `https`, `mailto` and Markdown files.
- Long code lines wrap, as there is no horizontal scrolling.
- See-through backgrounds need Wayland; on X11 the window stays opaque.

## How memory stays low

Only blocks near the viewport are shaped; the rest are height estimates. Long code
blocks are split into chunks, and table rows are separate blocks, so they lay out
lazily too. Glyphs and decoded images live in byte-limited caches, and system fonts
are scanned only when a document needs a character the embedded fonts lack.

The largest cost is usually the window's pixel buffers, at width × height × 4 bytes
each, with up to two in use.

## Development

```sh
cargo test
markdonk testdata/sample.md --render-png out.png --render-size 2000x1600 --render-scale 2
MARKDONK_BENCH=big.md cargo test --release frame_times -- --ignored --nocapture
```

`--render-png` paints one frame with no window, and `--render-scroll`, `--render-toc`
and `--render-search` set up what it shows.

## Fonts

Embeds subsets of [Inter](https://github.com/rsms/inter) and
[JetBrains Mono](https://github.com/JetBrains/JetBrainsMono), both under the SIL
Open Font License. `markdonk --licenses` prints the license texts.

## License

markdonk is under the [MIT License](LICENSE). The embedded fonts keep their own license,
the SIL Open Font License, with the texts in `assets/fonts/`.
