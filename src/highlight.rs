//! Small table-driven syntax highlighter. It scans lazily, one code chunk at a
//! time, so highlighting costs no memory until a chunk is about to be shown.

pub const COMMENT: u16 = 1;
pub const STRING: u16 = 2;
pub const NUMBER: u16 = 3;
pub const KEYWORD: u16 = 4;
pub const TYPE: u16 = 5;
pub const FUNC: u16 = 6;

struct Lang {
    names: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    multiline_strings: &'static [&'static str],
    single_quote_strings: bool,
    ignore_case: bool,
    keywords: &'static [&'static str],
}

const C_LIKE_COMMENT: &[&str] = &["//"];

static LANGS: &[Lang] = &[
    Lang {
        names: &["rust", "rs"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &[],
        single_quote_strings: false,
        ignore_case: false,
        keywords: &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
    },
    Lang {
        names: &["c", "h", "cpp", "c++", "cc", "cxx", "hpp", "objc"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr",
            "continue", "default", "delete", "do", "double", "else", "enum", "extern", "false",
            "float", "for", "goto", "if", "include", "inline", "int", "long", "namespace", "new",
            "nullptr", "private", "protected", "public", "return", "short", "signed", "sizeof",
            "static", "struct", "switch", "template", "this", "throw", "true", "try", "typedef",
            "typename", "union", "unsigned", "using", "virtual", "void", "volatile", "while",
        ],
    },
    Lang {
        names: &["go", "golang"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &["`"],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "false",
            "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map", "nil",
            "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
        ],
    },
    Lang {
        names: &["js", "javascript", "jsx", "ts", "typescript", "tsx", "mjs", "cjs"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &["`"],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
            "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false",
            "finally", "for", "from", "function", "if", "implements", "import", "in",
            "instanceof", "interface", "let", "new", "null", "of", "private", "protected",
            "public", "return", "static", "super", "switch", "this", "throw", "true", "try",
            "type", "typeof", "undefined", "var", "void", "while", "yield",
        ],
    },
    Lang {
        names: &["python", "py", "python3"],
        line_comments: &["#"],
        block_comment: None,
        multiline_strings: &["\"\"\"", "'''"],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
            "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
            "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
            "return", "self", "try", "while", "with", "yield",
        ],
    },
    Lang {
        names: &["bash", "sh", "shell", "zsh", "fish", "console"],
        line_comments: &["#"],
        block_comment: None,
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "case", "do", "done", "elif", "else", "end", "esac", "exit", "export", "fi", "for",
            "function", "if", "in", "local", "return", "set", "then", "until", "while",
        ],
    },
    Lang {
        names: &["java", "kotlin", "kt", "csharp", "cs", "c#", "swift", "scala", "dart"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &["\"\"\""],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "abstract", "as", "async", "await", "boolean", "break", "case", "catch", "class",
            "const", "continue", "data", "default", "do", "double", "else", "enum", "extends",
            "extension", "false", "final", "finally", "float", "for", "fun", "func", "guard", "if",
            "implements", "import", "in", "int", "interface", "is", "let", "long", "namespace",
            "new", "null", "object", "override", "package", "private", "protected", "public",
            "return", "sealed", "static", "string", "struct", "super", "switch", "this", "throw",
            "throws", "true", "try", "using", "val", "var", "void", "when", "while",
        ],
    },
    Lang {
        names: &["ruby", "rb", "lua", "elixir", "ex"],
        line_comments: &["#", "--"],
        block_comment: None,
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "and", "begin", "break", "case", "class", "def", "defmodule", "do", "else", "elseif",
            "elsif", "end", "ensure", "false", "for", "function", "if", "in", "local", "module",
            "next", "nil", "not", "or", "rescue", "return", "self", "then", "true", "unless",
            "until", "when", "while", "yield",
        ],
    },
    Lang {
        names: &["php", "zig", "css", "scss"],
        line_comments: C_LIKE_COMMENT,
        block_comment: Some(("/*", "*/")),
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[
            "break", "case", "catch", "class", "const", "continue", "defer", "echo", "else", "enum",
            "error", "false", "fn", "for", "foreach", "function", "if", "import", "null", "pub",
            "public", "return", "struct", "switch", "true", "try", "var", "while",
        ],
    },
    Lang {
        names: &["sql", "mysql", "postgresql", "postgres", "sqlite"],
        line_comments: &["--"],
        block_comment: Some(("/*", "*/")),
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: true,
        keywords: &[
            "alter", "and", "as", "asc", "begin", "between", "by", "case", "commit", "create",
            "delete", "desc", "distinct", "drop", "else", "end", "exists", "false", "foreign",
            "from", "group", "having", "if", "in", "index", "inner", "insert", "into", "is", "join",
            "key", "left", "like", "limit", "not", "null", "offset", "on", "or", "order", "outer",
            "primary", "references", "right", "rollback", "select", "set", "table", "then", "true",
            "union", "unique", "update", "values", "when", "where", "with",
        ],
    },
    Lang {
        names: &["json", "jsonc", "json5", "yaml", "yml", "toml", "ini", "dockerfile", "docker", "make", "makefile", "nix"],
        line_comments: &["#"],
        block_comment: None,
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &["false", "no", "null", "off", "on", "true", "yes"],
    },
    Lang {
        names: &["html", "xml", "svg", "vue", "svelte"],
        line_comments: &[],
        block_comment: Some(("<!--", "-->")),
        multiline_strings: &[],
        single_quote_strings: true,
        ignore_case: false,
        keywords: &[],
    },
];

const DIFF: u8 = 255;

/// Maps a fence label such as `rust` to a language id; 0 means no highlighting.
pub fn lang_id(name: &str) -> u8 {
    let name = name.split([' ', ',', '{']).next().unwrap_or("").to_ascii_lowercase();
    if name == "diff" || name == "patch" {
        return DIFF;
    }
    LANGS
        .iter()
        .position(|l| l.names.contains(&name.as_str()))
        .map_or(0, |i| i as u8 + 1)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Scans `text`, reporting `(start, end, token)` for each coloured range. The
/// returned state carries an open block comment or string into the next chunk.
pub fn scan(lang: u8, text: &str, state: u8, mut emit: impl FnMut(usize, usize, u16)) -> u8 {
    if lang == DIFF {
        let mut pos = 0;
        for line in text.split('\n') {
            let token = match line.as_bytes().first() {
                Some(b'+') => Some(STRING),
                Some(b'-') => Some(KEYWORD),
                Some(b'@') => Some(TYPE),
                _ => None,
            };
            if let Some(token) = token {
                emit(pos, pos + line.len(), token);
            }
            pos += line.len() + 1;
        }
        return 0;
    }
    if lang == 0 {
        return 0;
    }
    let lang = &LANGS[lang as usize - 1];
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut state = state;

    while i < bytes.len() {
        // state 1 is an open block comment, state 2+n an open multiline string.
        if state == 1 {
            let close = lang.block_comment.map_or("", |c| c.1);
            let end = find_from(text, i, close).map_or(bytes.len(), |e| {
                state = 0;
                e + close.len()
            });
            emit(i, end, COMMENT);
            i = end;
            continue;
        }
        if state >= 2 {
            let close = lang.multiline_strings.get(state as usize - 2).copied().unwrap_or("");
            let end = find_from(text, i, close).map_or(bytes.len(), |e| {
                state = 0;
                e + close.len()
            });
            emit(i, end, STRING);
            i = end;
            continue;
        }

        let rest = &text[i..];
        if lang.line_comments.iter().any(|c| rest.starts_with(c)) {
            let end = rest.find('\n').map_or(bytes.len(), |e| i + e);
            emit(i, end, COMMENT);
            i = end;
            continue;
        }
        if let Some((open, _)) = lang.block_comment {
            if rest.starts_with(open) {
                state = 1;
                emit(i, i + open.len(), COMMENT);
                i += open.len();
                continue;
            }
        }
        if let Some(n) = lang.multiline_strings.iter().position(|d| rest.starts_with(d)) {
            state = n as u8 + 2;
            let len = lang.multiline_strings[n].len();
            emit(i, i + len, STRING);
            i += len;
            continue;
        }

        let b = bytes[i];
        if b == b'"' || (b == b'\'' && lang.single_quote_strings) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b && bytes[j] != b'\n' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(bytes.len());
            let end = floor_char(text, end);
            emit(i, end, STRING);
            i = end;
        } else if b.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && (is_ident(bytes[j]) || bytes[j] == b'.') {
                j += 1;
            }
            emit(i, j, NUMBER);
            i = j;
        } else if is_ident(b) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            let word = &text[i..j];
            let keyword = if lang.ignore_case {
                lang.keywords.iter().any(|k| k.eq_ignore_ascii_case(word))
            } else {
                lang.keywords.contains(&word)
            };
            if keyword {
                emit(i, j, KEYWORD);
            } else if b.is_ascii_uppercase() && word.len() > 1 {
                emit(i, j, TYPE);
            } else if bytes.get(j) == Some(&b'(') {
                emit(i, j, FUNC);
            }
            i = j;
        } else {
            i += 1;
            while i < bytes.len() && !text.is_char_boundary(i) {
                i += 1;
            }
        }
    }
    state
}

fn find_from(text: &str, from: usize, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    text[from..].find(needle).map(|e| from + e)
}

fn floor_char(text: &str, mut i: usize) -> usize {
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(lang: &str, text: &str) -> Vec<(String, u16)> {
        let mut out = Vec::new();
        scan(lang_id(lang), text, 0, |s, e, t| out.push((text[s..e].to_string(), t)));
        out
    }

    #[test]
    fn rust_tokens() {
        let t = tokens("rust", "fn main() { let x = \"hi\"; } // done");
        assert!(t.contains(&("fn".into(), KEYWORD)));
        assert!(t.contains(&("main".into(), FUNC)));
        assert!(t.contains(&("\"hi\"".into(), STRING)));
        assert!(t.contains(&("// done".into(), COMMENT)));
    }

    #[test]
    fn block_comment_state_crosses_chunks() {
        let lang = lang_id("c");
        let state = scan(lang, "int a; /* open", 0, |_, _, _| {});
        assert_eq!(state, 1);
        let mut seen = Vec::new();
        let state = scan(lang, "still */ int b;", state, |s, e, t| seen.push((s, e, t)));
        assert_eq!(state, 0);
        assert_eq!(seen[0], (0, 8, COMMENT));
    }

    #[test]
    fn non_ascii_and_unterminated_strings_do_not_panic() {
        tokens("rust", "let s = \"héllo");
        tokens("python", "x = 'é\\");
        tokens("js", "é ñ 日本語 `open");
        tokens("diff", "+a\n-b\n@@ c");
    }

    #[test]
    fn unknown_language_emits_nothing() {
        assert!(tokens("nosuchlang", "fn main()").is_empty());
    }
}
