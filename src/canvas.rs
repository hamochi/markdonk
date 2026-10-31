//! Drawing primitives over the window's own pixel buffer (0x00RRGGBB). Painting
//! here directly avoids holding a second full-window buffer.

pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    clip: (i32, i32, i32, i32),
}

#[inline]
pub fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    let a = alpha + (alpha >> 7);
    let ia = 256 - a;
    let rb = ((src & 0xff00ff) * a + (dst & 0xff00ff) * ia) >> 8;
    let g = ((src & 0x00ff00) * a + (dst & 0x00ff00) * ia) >> 8;
    (rb & 0xff00ff) | (g & 0x00ff00)
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], w: i32, h: i32) -> Self {
        Canvas { px, w, h, clip: (0, 0, w, h) }
    }

    pub fn set_clip(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.clip = (x0.max(0), y0.max(0), x1.min(self.w), y1.min(self.h));
    }

    pub fn reset_clip(&mut self) {
        self.clip = (0, 0, self.w, self.h);
    }

    pub fn clear(&mut self, color: u32) {
        self.px.fill(color);
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, x1) = (x.max(cx0), x.saturating_add(w).min(cx1));
        let (y0, y1) = (y.max(cy0), y.saturating_add(h).min(cy1));
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            let start = (row * self.w + x0) as usize;
            self.px[start..start + (x1 - x0) as usize].fill(color);
        }
    }

    pub fn blend_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32, alpha: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, x1) = (x.max(cx0), x.saturating_add(w).min(cx1));
        let (y0, y1) = (y.max(cy0), y.saturating_add(h).min(cy1));
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            let start = (row * self.w + x0) as usize;
            for p in &mut self.px[start..start + (x1 - x0) as usize] {
                *p = blend(*p, color, alpha);
            }
        }
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, color: u32, alpha: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        if x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            let p = &mut self.px[(y * self.w + x) as usize];
            *p = if alpha >= 255 { color } else { blend(*p, color, alpha) };
        }
    }

    /// Filled rectangle with antialiased corners.
    pub fn rrect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: u32) {
        let r = radius.min(w / 2).min(h / 2).max(0);
        if r == 0 {
            self.rect(x, y, w, h, color);
            return;
        }
        self.rect(x, y + r, w, h - 2 * r, color);
        self.rect(x + r, y, w - 2 * r, r, color);
        self.rect(x + r, y + h - r, w - 2 * r, r, color);
        let rf = r as f32;
        for dy in 0..r {
            for dx in 0..r {
                let d = ((rf - dx as f32 - 0.5).powi(2) + (rf - dy as f32 - 0.5).powi(2)).sqrt();
                let coverage = (rf - d + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let a = (coverage * 255.0) as u32;
                    self.put(x + dx, y + dy, color, a);
                    self.put(x + w - 1 - dx, y + dy, color, a);
                    self.put(x + dx, y + h - 1 - dy, color, a);
                    self.put(x + w - 1 - dx, y + h - 1 - dy, color, a);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: u32) {
        let (x0, x1) = ((cx - radius - 1.0) as i32, (cx + radius + 2.0) as i32);
        let (y0, y1) = ((cy - radius - 1.0) as i32, (cy + radius + 2.0) as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let coverage = (radius - d + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.put(x, y, color, (coverage * 255.0) as u32);
                }
            }
        }
    }

    /// Antialiased thick line segment, used for the task checkmark.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: u32) {
        let half = width / 2.0;
        let x0 = (a.0.min(b.0) - half - 1.0) as i32;
        let x1 = (a.0.max(b.0) + half + 2.0) as i32;
        let y0 = (a.1.min(b.1) - half - 1.0) as i32;
        let y1 = (a.1.max(b.1) + half + 2.0) as i32;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(0.0001);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5 - a.0, y as f32 + 0.5 - a.1);
                let t = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
                let d = ((px - t * dx).powi(2) + (py - t * dy).powi(2)).sqrt();
                let coverage = (half - d + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.put(x, y, color, (coverage * 255.0) as u32);
                }
            }
        }
    }

    /// Blits an 8-bit coverage mask in a single colour.
    pub fn mask(&mut self, x: i32, y: i32, w: i32, h: i32, data: &[u8], color: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        for row in y.max(cy0)..(y + h).min(cy1) {
            let src = ((row - y) * w) as usize;
            let dst = (row * self.w) as usize;
            for col in x.max(cx0)..(x + w).min(cx1) {
                let a = data[src + (col - x) as usize] as u32;
                if a > 0 {
                    let p = &mut self.px[dst + col as usize];
                    *p = blend(*p, color, a);
                }
            }
        }
    }

    /// Blits straight-alpha RGBA bytes, as produced for colour glyphs.
    pub fn rgba_bytes(&mut self, x: i32, y: i32, w: i32, h: i32, data: &[u8]) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        for row in y.max(cy0)..(y + h).min(cy1) {
            let src = ((row - y) * w) as usize * 4;
            let dst = (row * self.w) as usize;
            for col in x.max(cx0)..(x + w).min(cx1) {
                let s = &data[src + (col - x) as usize * 4..][..4];
                if s[3] > 0 {
                    let color = (s[0] as u32) << 16 | (s[1] as u32) << 8 | s[2] as u32;
                    let p = &mut self.px[dst + col as usize];
                    *p = blend(*p, color, s[3] as u32);
                }
            }
        }
    }

    /// Blits 0xAARRGGBB pixels.
    pub fn argb(&mut self, x: i32, y: i32, w: i32, h: i32, data: &[u32]) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        for row in y.max(cy0)..(y + h).min(cy1) {
            let src = ((row - y) * w) as usize;
            let dst = (row * self.w) as usize;
            for col in x.max(cx0)..(x + w).min(cx1) {
                let s = data[src + (col - x) as usize];
                let a = s >> 24;
                let p = &mut self.px[dst + col as usize];
                *p = if a == 255 { s & 0xffffff } else { blend(*p, s & 0xffffff, a) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_are_exact() {
        assert_eq!(blend(0x123456, 0xabcdef, 0), 0x123456);
        assert_eq!(blend(0x123456, 0xabcdef, 255), 0xabcdef);
    }

    #[test]
    fn drawing_outside_the_canvas_is_clipped_not_a_panic() {
        let mut px = vec![0u32; 100];
        let mut c = Canvas::new(&mut px, 10, 10);
        c.rect(-5, -5, 100, 100, 1);
        c.rrect(-20, 8, 40, 40, 6, 2);
        c.circle(-3.0, 12.0, 8.0, 3);
        c.line((-5.0, -5.0), (20.0, 20.0), 3.0, 4);
        c.mask(8, 8, 4, 4, &[255; 16], 5);
        c.rgba_bytes(-2, -2, 4, 4, &[255; 64]);
        c.argb(9, 9, 2, 2, &[0xff000000; 4]);
        c.set_clip(2, 2, 4, 4);
        c.rect(0, 0, 10, 10, 9);
        assert_eq!(px[22], 9);
        assert_ne!(px[11], 9);
    }
}
