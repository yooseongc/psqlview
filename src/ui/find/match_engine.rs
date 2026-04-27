//! Literal-substring scanner. Char-position aware (i.e. UTF-8 safe).

/// Finds every char-position occurrence of `needle` in `line`.
/// Returns `(start_col, end_col)` pairs in char units. Matches don't
/// overlap — once a match lands, scanning resumes at
/// `start + needle.chars()`.
pub(crate) fn find_in_line(
    line: &str,
    needle: &str,
    case_insensitive: bool,
) -> Vec<(usize, usize)> {
    let line_chars: Vec<char> = line.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let mut out = Vec::new();
    let m = needle_chars.len();
    let n = line_chars.len();
    if m == 0 || m > n {
        return out;
    }
    let cmp = |a: char, b: char| -> bool {
        if case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    };
    let mut i = 0;
    while i + m <= n {
        let mut j = 0;
        while j < m && cmp(line_chars[i + j], needle_chars[j]) {
            j += 1;
        }
        if j == m {
            out.push((i, i + m));
            i += m;
        } else {
            i += 1;
        }
    }
    out
}
