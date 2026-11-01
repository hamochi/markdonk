//! Built-in pages, written as Markdown so they render like any other document:
//! the start page with recent files, and a keyboard-friendly file browser.

use std::path::{Path, PathBuf};

pub const BROWSE: &str = "markdonk:browse/";
const MAX_RECENT: usize = 20;
const MAX_ENTRIES: usize = 2000;
const EXTENSIONS: [&str; 6] = ["md", "markdown", "mdown", "mkd", "mdx", "txt"];

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\`*_[]<>&#~!|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn recent_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/markdonk/recent"))
}

fn merge_recent(existing: &str, path: &str) -> String {
    let mut out = String::with_capacity(existing.len() + path.len() + 1);
    out.push_str(path);
    out.push('\n');
    for line in existing.lines().filter(|l| *l != path && !l.is_empty()).take(MAX_RECENT - 1) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Records a file as recently opened. Failing to do so never matters.
pub fn remember(path: &Path) {
    let (Some(file), Ok(full)) = (recent_file(), std::fs::canonicalize(path)) else { return };
    let Some(full) = full.to_str().filter(|p| !p.contains('\n')).map(str::to_string) else { return };
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, merge_recent(&existing, &full));
}

fn recent_paths() -> Vec<PathBuf> {
    let text = recent_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    text.lines().map(PathBuf::from).filter(|p| p.is_file()).collect()
}

fn file_link(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("[{}]({})", escape_text(&name), encode_path(path))
}

pub fn start_page() -> String {
    start_page_from(&recent_paths())
}

fn start_page_from(recent: &[PathBuf]) -> String {
    let mut md = String::from("# markdonk\n\nPress `o` to browse for a file.\n");
    if !recent.is_empty() {
        md.push_str("\n## Recent files\n\n");
        for path in recent {
            let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            md.push_str(&format!("- {} in {}\n", file_link(path), escape_text(&dir)));
        }
    }
    md.push_str("\n`Tab` moves between links, `Enter` opens one, and `q` quits.\n");
    md
}

/// Lists folders and Markdown files in `dir`; hidden entries are left out.
pub fn browse_page(dir: &Path) -> String {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut truncated = false;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if dirs.len() + files.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
            } else if is_markdown(&path) {
                files.push(path);
            }
        }
    }
    dirs.sort();
    files.sort();
    let mut md = format!("# Open a file\n\n{}\n\n", escape_text(&dir.to_string_lossy()));
    if let Some(parent) = dir.parent() {
        md.push_str(&format!("- [..]({BROWSE}{})\n", encode_path(parent)));
    }
    for path in &dirs {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        md.push_str(&format!("- [{}/]({BROWSE}{})\n", escape_text(&name), encode_path(path)));
    }
    for path in &files {
        md.push_str(&format!("- {}\n", file_link(path)));
    }
    if dirs.is_empty() && files.is_empty() {
        md.push_str("\nNo folders or Markdown files here.\n");
    }
    if truncated {
        md.push_str(&format!("\nShowing the first {MAX_ENTRIES} entries.\n"));
    }
    md.push_str("\n`Tab` moves between links, `Enter` opens one, and `Backspace` goes back.\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc;

    #[test]
    fn recent_list_moves_a_file_to_the_top_without_duplicates() {
        assert_eq!(merge_recent("/b\n/a\n", "/a"), "/a\n/b\n");
        assert_eq!(merge_recent("", "/a"), "/a\n");
        let many: String = (0..50).map(|i| format!("/{i}\n")).collect();
        assert_eq!(merge_recent(&many, "/x").lines().count(), MAX_RECENT);
    }

    #[test]
    fn hostile_file_names_stay_plain_text_and_valid_links() {
        let path = Path::new("/tmp/a b/[x](http:evil)*.md");
        let d = doc::parse(&format!("- {}\n", file_link(path)), None);
        assert_eq!(&*d.blocks[0].text, "[x](http:evil)*.md");
        assert_eq!(d.links.len(), 1);
        assert_eq!(doc::resolve(None, &d.links[0]), path);
    }

    #[test]
    fn start_page_lists_recent_files_as_links() {
        let d = doc::parse(&start_page_from(&[PathBuf::from("/x/My Notes.md")]), None);
        assert!(d.blocks.iter().any(|b| b.text.starts_with("My Notes.md in /x")));
        assert_eq!(doc::resolve(None, &d.links[0]), Path::new("/x/My Notes.md"));
    }

    #[test]
    fn start_page_works_with_no_history() {
        assert!(!start_page_from(&[]).contains("Recent"));
    }

    #[test]
    fn browse_page_shows_folders_and_markdown_only() {
        let dir = std::env::temp_dir().join(format!("markdonk-browse-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("a.md"), "").unwrap();
        std::fs::write(dir.join("b.png"), "").unwrap();
        let md = browse_page(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(md.contains("a.md"));
        assert!(md.contains("sub/"));
        assert!(md.contains(&format!("{BROWSE}{}", encode_path(dir.parent().unwrap()))));
        assert!(!md.contains("b.png"));
        assert!(!md.contains("hidden"));
    }

    #[test]
    fn a_missing_folder_is_just_empty() {
        assert!(browse_page(Path::new("/no/such/dir")).contains("No folders"));
    }
}
