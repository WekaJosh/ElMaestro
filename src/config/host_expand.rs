//! Bash-style brace expansion for the Workers/hosts field.
//!
//! Supported forms (composable in any combination):
//!   - integer sequences:   `{1..100}`, `{1..10..2}` (with step)
//!   - zero-padded ranges:  `{01..16}` → "01", "02", ..., "16"
//!   - char ranges:         `{a..z}`, `{A..Z}`
//!   - comma lists:         `{foo,bar,baz}`
//!   - prefix + suffix:     `node{1..5}-eth0` → "node1-eth0", ...
//!   - cartesian product:   `10.10.{1..3}.{1..4}` → 12 hosts
//!
//! Top-level comma-separated entries are independent. Commas INSIDE
//! braces stay inside (they're alternatives). Each expanded host may
//! still carry a `:port` suffix — the brace expander is agnostic.
//!
//! Unbalanced braces, malformed ranges, or otherwise unrecognized
//! brace bodies are left unexpanded (the original substring is kept).
//! This matches bash's behavior and keeps the parser forgiving.

/// Expand a user-typed hosts string into a flat list of host expressions
/// (each may still need `:port` parsing by the caller). Trims whitespace,
/// drops empty entries, preserves input order.
pub fn expand_hosts(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in split_top_level_commas(input) {
        for expanded in expand_one(&entry) {
            let t = expanded.trim().to_string();
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    out
}

/// Recursively expand the first brace pair found in `s`, then recurse on
/// the result. Multiple braces in the same string produce the cartesian
/// product naturally because each alternative is independently re-expanded.
fn expand_one(s: &str) -> Vec<String> {
    let Some(open) = first_top_level_open_brace(s) else {
        return vec![s.to_string()];
    };
    let Some(close_rel) = matching_close(&s[open..]) else {
        // Unbalanced — leave the whole string as a literal.
        return vec![s.to_string()];
    };
    let close = open + close_rel;
    let prefix = &s[..open];
    let inside = &s[open + 1..close];
    let suffix = &s[close + 1..];

    let alternatives = expand_alternatives(inside);
    if alternatives.is_empty() {
        // Brace body didn't parse to anything useful; keep as a literal.
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    for alt in alternatives {
        let combined = format!("{}{}{}", prefix, alt, suffix);
        out.extend(expand_one(&combined));
    }
    out
}

fn first_top_level_open_brace(s: &str) -> Option<usize> {
    // First '{' — depth doesn't matter for the FIRST one. matching_close
    // handles depth from there.
    s.find('{')
}

/// `s` starts with `{`. Returns the index of the matching `}` (relative
/// to s) or None if unbalanced.
fn matching_close(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Expand the body of one brace pair into a list of alternatives.
fn expand_alternatives(inside: &str) -> Vec<String> {
    if let Some(seq) = parse_int_sequence(inside) {
        return seq;
    }
    if let Some(seq) = parse_char_range(inside) {
        return seq;
    }
    // Fallback: comma list. Empty entries are preserved (matches bash:
    // `{a,,b}` → "a", "", "b") — caller trims/drops at the very top.
    split_top_level_commas(inside)
}

/// `{N..M}` or `{N..M..S}`. Detects zero-padding from the first / last
/// number's apparent width.
fn parse_int_sequence(s: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = s.split("..").collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let raw_start = parts[0];
    let raw_end = parts[1];
    let n: i64 = raw_start.parse().ok()?;
    let m: i64 = raw_end.parse().ok()?;
    let step: i64 = if parts.len() == 3 {
        let s: i64 = parts[2].parse().ok()?;
        if s == 0 {
            return None;
        }
        s.abs() * if n <= m { 1 } else { -1 }
    } else if n <= m {
        1
    } else {
        -1
    };
    let pad = detect_pad_width(raw_start, raw_end);
    let mut out = Vec::new();
    let mut i = n;
    if step > 0 {
        while i <= m {
            out.push(format_padded(i, pad));
            i = i.saturating_add(step);
            if step > 0 && i < n {
                break; // overflow guard
            }
        }
    } else {
        while i >= m {
            out.push(format_padded(i, pad));
            i = i.saturating_add(step);
            if step < 0 && i > n {
                break;
            }
        }
    }
    Some(out)
}

/// Width to zero-pad sequence values to. Triggered when either endpoint
/// has a literal leading zero (e.g. "01", "008"). Width is max of both
/// endpoints' literal widths.
fn detect_pad_width(raw_start: &str, raw_end: &str) -> usize {
    let leading_zero = |s: &str| {
        let body = s.strip_prefix('-').unwrap_or(s);
        body.len() > 1 && body.starts_with('0')
    };
    if leading_zero(raw_start) || leading_zero(raw_end) {
        // Pad to the wider of the two literal widths (sign excluded).
        let w = |s: &str| s.strip_prefix('-').unwrap_or(s).len();
        w(raw_start).max(w(raw_end))
    } else {
        0
    }
}

fn format_padded(i: i64, width: usize) -> String {
    if width == 0 {
        i.to_string()
    } else if i < 0 {
        format!("-{:0w$}", -i, w = width.saturating_sub(1))
    } else {
        format!("{:0w$}", i, w = width)
    }
}

/// Single-char range like `{a..z}` or `{A..D}`. Only ASCII letters.
fn parse_char_range(s: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = s.split("..").collect();
    if parts.len() != 2 {
        return None;
    }
    if parts[0].len() != 1 || parts[1].len() != 1 {
        return None;
    }
    let a = parts[0].as_bytes()[0];
    let b = parts[1].as_bytes()[0];
    if !a.is_ascii_alphabetic() || !b.is_ascii_alphabetic() {
        return None;
    }
    let mut out = Vec::new();
    if a <= b {
        for c in a..=b {
            out.push((c as char).to_string());
        }
    } else {
        let mut c = a;
        while c >= b {
            out.push((c as char).to_string());
            if c == 0 {
                break;
            }
            c -= 1;
        }
    }
    Some(out)
}

/// Split `s` on `,` but ignore commas inside `{...}`.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '{' => {
                depth += 1;
                current.push(c);
            }
            '}' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    out.push(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_input_passes_through() {
        assert_eq!(expand_hosts("a"), vec!["a"]);
        assert_eq!(expand_hosts("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(expand_hosts("worker-01,worker-02"), vec!["worker-01", "worker-02"]);
    }

    #[test]
    fn integer_sequence() {
        assert_eq!(expand_hosts("h{1..3}"), vec!["h1", "h2", "h3"]);
    }

    #[test]
    fn ip_range() {
        let v = expand_hosts("10.10.10.{1..100}");
        assert_eq!(v.len(), 100);
        assert_eq!(v[0], "10.10.10.1");
        assert_eq!(v[99], "10.10.10.100");
    }

    #[test]
    fn zero_padded_sequence() {
        assert_eq!(
            expand_hosts("node{01..05}"),
            vec!["node01", "node02", "node03", "node04", "node05"]
        );
    }

    #[test]
    fn sequence_with_step() {
        assert_eq!(expand_hosts("h{1..10..2}"), vec!["h1", "h3", "h5", "h7", "h9"]);
    }

    #[test]
    fn descending_sequence() {
        assert_eq!(expand_hosts("h{5..1}"), vec!["h5", "h4", "h3", "h2", "h1"]);
    }

    #[test]
    fn comma_list() {
        assert_eq!(
            expand_hosts("gw{a,b,c}"),
            vec!["gwa", "gwb", "gwc"]
        );
    }

    #[test]
    fn char_range() {
        assert_eq!(
            expand_hosts("h{a..d}"),
            vec!["ha", "hb", "hc", "hd"]
        );
    }

    #[test]
    fn cartesian_product() {
        // 10.10.{1..2}.{1..2} → 4 IPs in row-major order.
        let v = expand_hosts("10.10.{1..2}.{1..3}");
        assert_eq!(
            v,
            vec![
                "10.10.1.1", "10.10.1.2", "10.10.1.3",
                "10.10.2.1", "10.10.2.2", "10.10.2.3",
            ]
        );
    }

    #[test]
    fn mixed_with_plain_entries() {
        let v = expand_hosts("10.10.10.{1..3},backup-01");
        assert_eq!(v, vec!["10.10.10.1", "10.10.10.2", "10.10.10.3", "backup-01"]);
    }

    #[test]
    fn comma_inside_braces_does_not_split_entries() {
        // The comma inside {a,b} must stay inside; the top-level split
        // must only see one entry "gw{a,b}".
        let v = expand_hosts("gw{a,b}");
        assert_eq!(v, vec!["gwa", "gwb"]);
    }

    #[test]
    fn port_suffix_preserved() {
        // Brace expander shouldn't touch :port. The Workers parser still
        // does its own :port split per expanded entry.
        let v = expand_hosts("worker{01..03}:1612");
        assert_eq!(v, vec!["worker01:1612", "worker02:1612", "worker03:1612"]);
    }

    #[test]
    fn unbalanced_braces_left_alone() {
        assert_eq!(expand_hosts("h{1..3"), vec!["h{1..3"]);
        assert_eq!(expand_hosts("h1..3}"), vec!["h1..3}"]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(expand_hosts("").is_empty());
        assert!(expand_hosts("   ").is_empty());
        assert!(expand_hosts(",,,").is_empty());
    }

    #[test]
    fn whitespace_trimmed_per_entry() {
        let v = expand_hosts("  a  ,  b  ");
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn unrecognized_brace_body_left_literal() {
        // Not a sequence, not a char range, not a comma list with >1
        // entry — passes through the comma-list path which yields the
        // body verbatim.
        let v = expand_hosts("h{abc}");
        assert_eq!(v, vec!["habc"]);
    }
}
