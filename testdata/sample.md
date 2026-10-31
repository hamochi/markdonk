# Markdonk rendering sample

A paragraph with **bold**, *italic*, ***both***, `inline code`, ~~struck~~ text and a [link to Rust](https://www.rust-lang.org). Bare URLs work too: https://example.com/page. Typography should wrap cleanly at the content width, keeping a comfortable reading measure on wide windows.

## Lists

- First bullet
- Second bullet with `code`
  - Nested item
    - Deeper item
- Third bullet

1. Ordered one
2. Ordered two
10. Ordered ten

- [x] Finished task
- [ ] Open task

## Code

```rust
// Fibonacci, with an iterator
fn fib(n: u32) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        (a, b) = (b, a + b);
    }
    a // "done"
}
```

```diff
- removed line
+ added line
@@ hunk @@
```

## Tables

| Feature | Status | Size |
|:--------|:------:|-----:|
| Tables | **done** | 1 |
| Alignment | `ok` | 22 |
| Wrapping long cells | This cell has enough text in it to have to wrap across several lines in the column | 333 |

## Quotes

> Block quotes get a bar.
>
> > And they nest.

> [!WARNING]
> GitHub alerts have a title.

---

### Images

![Gradient](gradient.png)

![Remote badge](https://example.com/badge.svg)

<div align="center">Raw <b>HTML</b> text stays<br>and breaks work.</div>

## Unicode

Accents: café naïve Ελληνικά Русский. Symbols → ✓ ★. CJK: 日本語のテキスト. Emoji: 🎉🚀.

Footnotes work too.[^one]

[^one]: This is the footnote text.
