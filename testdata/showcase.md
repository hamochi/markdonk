![Markdonk, the donkey](donkey.png)

# Markdonk

A small, fast **Markdown viewer and editor** that draws everything itself: no browser engine, no GPU, and *no fuss*. It looks like the rest of your desktop, because it uses your theme.

> Press `e` to edit in place. The block under your caret shows raw Markdown, and everything else stays beautifully rendered.

## Why a donkey?

Because it carries a lot and asks for very little.

- **One binary**, with its fonts inside
- **Live reload** whenever the file changes
- **Your colours**, following your Omarchy theme
  - Switch themes and open windows recolour instantly
  - Your monospace font comes along too

## What's on the to-do list

- [x] Render Markdown properly
- [x] Edit it in place
- [x] Find a good donkey
- [ ] Bring carrots

## Measured, not guessed

| Task | Result | Target |
|:-----|:------:|-------:|
| First paint | **about 30 ms** | under 100 ms |
| Painting a frame | about 9 ms | smooth |
| Idle CPU | none | none |

## A little code

```rust
// The whole philosophy, more or less.
fn render(doc: &Doc, viewport: Range<usize>) {
    for block in visible_blocks(doc, viewport) {
        block.shape_if_needed();
        block.paint();
    }
}
```

Search with `/`, jump around with `t`, and go back with `Backspace`. Read more at [the project page](https://github.com/hamochi/markdonk).
