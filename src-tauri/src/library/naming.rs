//! Episode naming heuristics: sort keys + display names. No external deps.

/// Extract an episode number from a file/folder name. Handles common anime
/// patterns: `[04]`, `EP-12`, `S02E07`, `- 08 `, `第09話`, trailing numbers.
pub fn parse_episode_number(name: &str) -> Option<f64> {
    let lower = name.to_lowercase();

    // SxxExx
    if let Some(idx) = find_regex_like(&lower, 's', 'e') {
        let (s, e) = idx;
        if let (Some(sv), Some(ev)) = (parse_num(&lower[s]), parse_num(&lower[e])) {
            return Some(sv * 1000.0 + ev);
        }
    }

    // explicit markers: ep / episode / 话 / 話 / ch
    for marker in ["episode", "ep.", "ep ", "ep-", "ep", "ch.", "話", "话"] {
        if let Some(pos) = lower.find(marker) {
            let rest = &name[pos + marker.len()..];
            if let Some(n) = first_number(rest.trim_start_matches([' ', '-', '_', '#', ':'])) {
                return Some(n);
            }
        }
    }

    // bracketed number [04] or (04)
    for (open, close) in [('[', ']'), ('(', ')')] {
        if let (Some(a), Some(b)) = (name.find(open), name.rfind(close)) {
            if b > a {
                let inner = &name[a + 1..b];
                if let Some(n) = first_number(inner.trim()) {
                    return Some(n);
                }
            }
        }
    }

    // last standalone number that is not part of a year/resolution token
    let mut best: Option<f64> = None;
    let chars: Vec<char> = name.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                j += 1;
            }
            let tok: String = chars[i..j].iter().collect();
            if let Ok(v) = tok.parse::<f64>() {
                let before = if i > 0 { chars[i - 1] } else { ' ' };
                let after = if j < chars.len() { chars[j] } else { ' ' };
                let delim_ok = !before.is_alphanumeric() && !after.is_alphanumeric();
                if delim_ok && v < 10000.0 {
                    best = Some(v);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    best
}

fn find_regex_like(s: &str, sc: char, ec: char) -> Option<(usize, usize)> {
    // find pattern s+digits+e+digits
    let bytes: Vec<char> = s.chars().collect();
    for i in 0..bytes.len() {
        if bytes[i] == sc {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == ec {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    return Some((i + 1, j + 1));
                }
            }
        }
    }
    None
}

fn parse_num(s: &str) -> Option<f64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn first_number(s: &str) -> Option<f64> {
    let trimmed = s.trim_start();
    let digits: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Sort key used by the DB; files without a detected number sort last via NULL.
pub fn episode_sort_key(file_name: &str) -> Option<f64> {
    parse_episode_number(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_patterns() {
        assert_eq!(parse_episode_number("[Sub][Show][04][720p].mkv"), Some(4.0));
        assert_eq!(parse_episode_number("Frieren - EP-12.mkv"), Some(12.0));
        assert_eq!(parse_episode_number("Anime.S02E07.1080p.mp4"), Some(2007.0));
        assert_eq!(parse_episode_number("Show Title - 08 [WEB].ass"), Some(8.0));
        assert_eq!(parse_episode_number("第09話.mp4"), Some(9.0));
        assert_eq!(parse_episode_number("episode_103_final.mkv"), Some(103.0));
    }

    #[test]
    fn ordering_is_stable_for_double_digits() {
        let a = episode_sort_key("Episode 2.mkv").unwrap();
        let b = episode_sort_key("Episode 10.mkv").unwrap();
        assert!(a < b);
    }
}
