//! Text resources. Embedded fonts are served straight from the binary's pages;
//! system fonts are only scanned once a document needs a character the embedded
//! fonts lack, so most documents never pay for them.

use cosmic_text::fontdb::{Database, Source};
use cosmic_text::{CacheKey, Font, FontSystem, SwashCache, SwashContent, Weight};
use std::collections::HashMap;
use std::sync::Arc;

pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";

const EMBEDDED: [&[u8]; 7] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
];

const MAX_GLYPH_BYTES: usize = 8 * 1024 * 1024;
const LOCALE: &str = "en-US";

pub struct Glyph {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub color: bool,
    pub data: Box<[u8]>,
}

pub struct Fonts {
    pub fs: FontSystem,
    swash: SwashCache,
    glyphs: HashMap<CacheKey, Option<Glyph>>,
    glyph_bytes: usize,
    system_loaded: bool,
    embedded: [Arc<Font>; 2],
    /// Family used for code and the raw editing block.
    pub mono: &'static str,
}

impl Fonts {
    pub fn new() -> Fonts {
        let mut db = Database::new();
        for data in EMBEDDED {
            db.load_font_source(Source::Binary(Arc::new(data)));
        }
        let mut fs = FontSystem::new_with_locale_and_db(LOCALE.into(), db);
        let embedded = [SANS, MONO].map(|family| {
            let id = fs
                .db()
                .faces()
                .find(|f| f.families.iter().any(|(name, _)| name == family))
                .map(|f| f.id)
                .expect("embedded font is in the database");
            fs.get_font(id, Weight::NORMAL).expect("embedded font parses")
        });
        Fonts {
            fs,
            swash: SwashCache::new(),
            glyphs: HashMap::new(),
            glyph_bytes: 0,
            system_loaded: false,
            embedded,
            mono: MONO,
        }
    }

    /// Loads system fonts the first time `text` needs one.
    pub fn ensure_coverage(&mut self, text: &str) {
        if self.system_loaded || text.is_ascii() {
            return;
        }
        let missing = text.chars().any(|c| {
            !c.is_control() && self.embedded.iter().all(|f| f.as_swash().charmap().map(c) == 0)
        });
        if missing {
            self.load_system_fonts();
        }
    }

    fn load_system_fonts(&mut self) {
        if self.system_loaded {
            return;
        }
        self.system_loaded = true;
        let placeholder = FontSystem::new_with_locale_and_db(LOCALE.into(), Database::new());
        let fs = std::mem::replace(&mut self.fs, placeholder);
        let (locale, mut db) = fs.into_locale_and_db();
        db.load_system_fonts();
        self.fs = FontSystem::new_with_locale_and_db(locale, db);
    }

    /// Follows the system monospace font. Returns whether the family changed.
    /// JetBrains Mono variants map to the embedded copy, so that common case
    /// never scans system fonts; icon glyphs still fall back when they're needed.
    pub fn set_mono(&mut self, family: &str) -> bool {
        let squashed: String = family.chars().filter(|c| !c.is_whitespace()).collect();
        let wanted: &'static str = if squashed.to_ascii_lowercase().starts_with("jetbrainsmono") {
            MONO
        } else {
            self.load_system_fonts();
            let known = self.fs.db().faces().any(|f| f.families.iter().any(|(name, _)| name == family));
            if !known {
                return false;
            }
            if family == self.mono {
                return false;
            }
            // A handful of bytes, once per font change.
            String::leak(family.to_string())
        };
        let changed = wanted != self.mono;
        self.mono = wanted;
        changed
    }

    /// Rasterises through a byte-bounded cache that resets when full.
    pub fn glyph(&mut self, key: CacheKey) -> Option<&Glyph> {
        if self.glyph_bytes > MAX_GLYPH_BYTES {
            self.glyphs = HashMap::new();
            self.glyph_bytes = 0;
        }
        let (fs, swash, bytes) = (&mut self.fs, &mut self.swash, &mut self.glyph_bytes);
        self.glyphs
            .entry(key)
            .or_insert_with(|| {
                let image = swash.get_image_uncached(fs, key)?;
                if matches!(image.content, SwashContent::SubpixelMask) {
                    return None;
                }
                *bytes += image.data.len() + 64;
                Some(Glyph {
                    left: image.placement.left,
                    top: image.placement.top,
                    width: image.placement.width as i32,
                    height: image.placement.height as i32,
                    color: matches!(image.content, SwashContent::Color),
                    data: image.data.into_boxed_slice(),
                })
            })
            .as_ref()
    }
}
