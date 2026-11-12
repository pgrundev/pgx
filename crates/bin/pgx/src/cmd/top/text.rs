//! Terminal-safe, width-aware text for `pgx top`.
//!
//! Every label that reaches the screen (database names, project names, Git
//! branches, task labels, error messages) passes through [`sanitize`] first:
//! control characters (C0, DEL, C1 — which includes ESC and CSI, so no ANSI
//! sequence survives) and bidirectional overrides are replaced by `?`.
//! [`clip`] and [`fit`] then cut by *display width* (wide CJK glyphs count
//! two, combining marks zero), so a long or exotic label can never push a
//! column out of its cell.
use unicode_width::UnicodeWidthChar;

/// Characters that can reorder or hide text on a terminal (Trojan Source):
/// the bidi embeddings/overrides/isolates and the invisible marks.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// A label safe to print: control and bidi characters become `?`.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || is_bidi_control(c) {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// True when `s` has nothing [`sanitize`] would replace.
pub fn is_clean(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || is_bidi_control(c))
}

/// Display width of a sanitized string.
pub fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// At most `max` columns of `s` (sanitized); a cut is marked with `…`.
pub fn clip(s: &str, max: usize) -> String {
    let s = sanitize(s);
    if width(&s) <= max {
        return s;
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Exactly `cols` columns: clipped, then padded with spaces (left-aligned).
pub fn fit(s: &str, cols: usize) -> String {
    let mut out = clip(s, cols);
    let w = width(&out);
    for _ in w..cols {
        out.push(' ');
    }
    out
}

/// Exactly `cols` columns, right-aligned.
pub fn fit_right(s: &str, cols: usize) -> String {
    let c = clip(s, cols);
    let w = width(&c);
    let mut out = String::with_capacity(cols);
    for _ in w..cols {
        out.push(' ');
    }
    out.push_str(&c);
    out
}

/// `12m`, `1h42m`, `3d4h`, `45s` — the compact age/TTL form of the table.
pub fn compact_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    if d > 0 {
        if h > 0 {
            format!("{d}d{h}h")
        } else {
            format!("{d}d")
        }
    } else if h > 0 {
        if m > 0 {
            format!("{h}h{m:02}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// `00:42:18` (or `3d 04:05:06`), for the runtime uptime.
pub fn clock_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

/// `192 MB`, `7.2 kB`, `512 B`.
pub fn bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Local wall-clock `HH:MM:SS` of an epoch second (UTC if the C library
/// cannot convert).
pub fn local_hms(epoch: u64) -> String {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if ok {
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        let s = epoch % 86_400;
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_escapes_controls_and_bidi() {
        assert_eq!(sanitize("plain-name_1"), "plain-name_1");
        assert_eq!(sanitize("\x1b[31mred\x1b[0m"), "?[31mred?[0m");
        assert_eq!(sanitize("a\nb\tc\rd\x07"), "a?b?c?d?");
        assert_eq!(sanitize("x\u{9b}2Jy"), "x?2Jy", "C1 CSI");
        assert_eq!(sanitize("x\u{7f}y"), "x?y");
        assert_eq!(sanitize("abc\u{202E}fed"), "abc?fed");
        assert_eq!(sanitize("\u{2066}x\u{2069}"), "?x?");
        assert!(!sanitize("\x1b]0;title\x07").contains('\x1b'));
        assert!(is_clean("ok ünïcødé 漢字"));
        assert!(!is_clean("bad\x1b"));
        assert!(!is_clean("bad\u{202E}"));
    }

    #[test]
    fn widths_count_wide_and_combining_glyphs() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("漢字"), 4);
        assert_eq!(width("e\u{301}"), 1, "combining acute is zero-width");
        assert_eq!(width("🚀"), 2);
    }

    #[test]
    fn clip_and_fit_never_exceed_their_cell() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abc", 0), "");
        assert_eq!(clip("abc", 1), "…");
        // A wide glyph that would straddle the edge is dropped, not split.
        let c = clip("ab漢字cd", 4);
        assert!(width(&c) <= 4, "{c:?}");
        assert_eq!(c, "ab…");
        for s in [
            "x",
            "漢字漢字漢字",
            "e\u{301}e\u{301}e\u{301}e\u{301}",
            "\x1b[2J\x1b[H boom",
            "🚀🚀🚀🚀",
            "",
        ] {
            for cols in 0..12 {
                let f = fit(s, cols);
                assert_eq!(width(&f), cols, "fit({s:?}, {cols}) = {f:?}");
                assert!(is_clean(&f));
                let r = fit_right(s, cols);
                assert_eq!(width(&r), cols, "fit_right({s:?}, {cols}) = {r:?}");
            }
        }
    }

    #[test]
    fn compact_durations_and_sizes() {
        assert_eq!(compact_secs(0), "0s");
        assert_eq!(compact_secs(45), "45s");
        assert_eq!(compact_secs(12 * 60), "12m");
        assert_eq!(compact_secs(3600 + 42 * 60), "1h42m");
        assert_eq!(compact_secs(2 * 3600), "2h");
        assert_eq!(compact_secs(3 * 86_400 + 4 * 3600), "3d4h");
        assert_eq!(compact_secs(86_400), "1d");
        assert_eq!(clock_secs(42 * 60 + 18), "00:42:18");
        assert_eq!(clock_secs(86_400 + 3661), "1d 01:01:01");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(201_326_592), "192 MB");
        assert_eq!(bytes(7_500_000), "7.2 MB");
        assert_eq!(local_hms(0).len(), 8);
    }
}
