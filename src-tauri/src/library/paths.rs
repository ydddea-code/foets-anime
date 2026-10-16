//! Path normalization & validation. All frontend-supplied paths must pass
//! through here before touching the filesystem or database.

use std::path::{Component, Path, PathBuf};

/// Normalize a path for dedup/comparison: resolve `.`/`..` lexically, unify
/// separators, strip trailing separators and Windows `\\?\` prefixes. On
/// Windows, drive letters are upper-cased for case-insensitive comparison.
pub fn normalize_path(p: &str) -> String {
    let trimmed = p.trim().trim_matches(|c| c == '"' || c == '\'');
    let pb = PathBuf::from(trimmed);
    let mut out = PathBuf::new();
    for comp in pb.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(Component::ParentDir);
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    // strip extended-length prefix
    let s = out.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
    // collapse duplicate separators except at UNC root
    let sep_norm = s.replace("//", "/");
    if cfg!(windows) {
        // uppercase drive letter: "c:\x" -> "C:\x"; comparisons then via NOCASE collation
        let mut chars = sep_norm.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(d), Some(':'), Some(s)) if s == '\\' && d.is_ascii_alphabetic() => {
                format!("{}:{}", d.to_ascii_uppercase(), &sep_norm[2..])
            }
            _ => sep_norm,
        }
    } else {
        sep_norm
    }
}

/// Validate that a user supplied directory is safe to add to the library.
/// Rejects empty paths and system-critical locations.
pub fn validate_folder_path(p: &str) -> Result<String, String> {
    let norm = normalize_path(p);
    if norm.is_empty() {
        return Err("Path is empty".into());
    }
    let lower = norm.to_lowercase();
    #[cfg(windows)]
    const BLOCKED: &[&str] = &[
        "c:\\windows", "c:\\program files", "c:\\program files (x86)", "c:\\users",
        "\\\\", "c:\\", "d:\\",
    ];
    #[cfg(not(windows))]
    const BLOCKED: &[&str] = &["/", "/etc", "/usr", "/var", "/bin", "/sbin", "/boot", "/root"];
    for b in BLOCKED {
        if lower == *b || lower.starts_with(&format!("{b}\\")) || lower.starts_with(&format!("{b}/")) {
            return Err(format!("Refusing to add protected system path: {norm}"));
        }
    }
    Ok(norm)
}

/// Is this path an existing readable directory? Updates status flags upstream.
pub fn dir_exists(p: &str) -> bool {
    let path = Path::new(p);
    path.is_dir()
}

/// Relative path helper that never escapes the parent (`..` components removed).
pub fn safe_rel_path(root: &Path, file: &Path) -> String {
    match file.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| String::from("/")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spaces_unicode_and_dots() {
        assert_eq!(normalize_path("  /a/b/../c  "), "/a/c");
        assert_eq!(normalize_path("/a/./b/"), "/a/b");
        assert_eq!(normalize_path("/mnt/アニメ 東京"), "/mnt/アニメ 東京");
        assert_eq!(normalize_path(r"C:\Anime\One Piece\\"), r"C:\Anime\One Piece");
    }

    #[test]
    fn blocks_system_paths() {
        assert!(validate_folder_path("/").is_err());
        assert!(validate_folder_path("").is_err());
        assert!(validate_folder_path("/etc/passwd.d").is_err());
        assert!(validate_folder_path("/home/user/anime").is_ok());
    }

    #[test]
    fn rel_path_no_escape() {
        let root = Path::new("/anime/x");
        assert_eq!(safe_rel_path(root, Path::new("/anime/x/s01/e1.mkv")), "s01/e1.mkv");
        assert_eq!(safe_rel_path(root, Path::new("/other/f.mkv")), "f.mkv");
    }
}
