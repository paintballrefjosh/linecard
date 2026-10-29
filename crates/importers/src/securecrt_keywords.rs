//! SecureCRT keyword lists (`Config/Keywords/<name>.ini`).
//!
//! ```text
//! S:"List Name"=Cisco Words
//! D:"Match Case"=00000000
//! Z:"Keyword List V2"=00000047
//!  "(?:yes|permit|\[OK\])",0073c372,00000001
//! Z:"Keyword List V3"=00000047
//!  "(?:yes|permit|\[OK\])",0073c372,00000001,00000001
//! ```
//!
//! Each entry is the pattern, a Windows COLORREF (`00BBGGRR`) and a flag
//! that marks it as a regular expression; V3 adds a field we don't need.
//! Entries whose pattern starts with `[*]` are labels by convention (the
//! regex only matches a literal `*…`), so they are skipped.

use std::path::{Path, PathBuf};

use linecard_core_model::{HighlightProfile, HighlightRule};

/// Where SecureCRT keeps keyword lists on this computer, if it's installed.
pub fn default_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("VanDyke/Config/Keywords");
    dir.is_dir().then_some(dir)
}

/// The `.ini` files in `dir`, sorted by name.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ini")))
        .collect();
    out.sort();
    out
}

/// Parse one keyword list. `fallback_name` (usually the file name) is used
/// when the file has no "List Name". The returned profile has an empty id.
pub fn parse(text: &str, fallback_name: &str) -> Result<HighlightProfile, String> {
    let text = text.trim_start_matches('\u{feff}');
    let mut name = None;
    let mut match_case = false;
    let mut plain_words = String::new();
    // (fields after the pattern, entries) per section; V3 is preferred.
    let mut v2 = Vec::new();
    let mut v3 = Vec::new();
    let mut section: Option<(usize, &mut Vec<HighlightRule>)> = None;

    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        if let Some(entry) = line.strip_prefix(' ') {
            if let Some((fields, rules)) = section.as_mut() {
                if let Some(rule) = parse_entry(entry, *fields) {
                    rules.push(rule);
                }
            }
            continue;
        }
        section = None;
        if let Some(v) = line.strip_prefix("S:\"List Name\"=") {
            name = Some(v.trim().to_string()).filter(|n| !n.is_empty());
        } else if let Some(v) = line.strip_prefix("D:\"Match Case\"=") {
            match_case = u32::from_str_radix(v.trim(), 16).is_ok_and(|n| n != 0);
        } else if let Some(v) = line.strip_prefix("S:\"Keyword List\"=") {
            plain_words = v.to_string();
        } else if line.starts_with("Z:\"Keyword List V3\"=") {
            section = Some((3, &mut v3));
        } else if line.starts_with("Z:\"Keyword List V2\"=") {
            section = Some((2, &mut v2));
        }
    }

    let mut rules = if !v3.is_empty() { v3 } else { v2 };
    if rules.is_empty() {
        // Lists from old versions: plain words, no colors.
        rules = plain_words
            .split([',', ';', ' ', '\t'])
            .filter(|w| !w.is_empty())
            .map(|w| HighlightRule {
                text: w.to_string(),
                color: "amber".into(),
                regex: false,
            })
            .collect();
    }
    if rules.is_empty() && name.is_none() {
        return Err("this doesn't look like a SecureCRT keyword list".into());
    }
    Ok(HighlightProfile {
        id: String::new(),
        name: name.unwrap_or_else(|| fallback_name.to_string()),
        match_case,
        rules,
    })
}

/// ` "pattern",00BBGGRR,0000000F[,…]` with `fields` hex fields after the
/// pattern. The pattern may itself contain quotes and commas, so the hex
/// fields are split off the end.
fn parse_entry(entry: &str, fields: usize) -> Option<HighlightRule> {
    let mut parts = entry.trim().rsplitn(fields + 1, ',');
    let mut hex: Vec<u32> = Vec::with_capacity(fields);
    for _ in 0..fields {
        hex.push(u32::from_str_radix(parts.next()?.trim(), 16).ok()?);
    }
    hex.reverse();
    let pattern = parts.next()?.strip_prefix('"')?.strip_suffix('"')?;
    if pattern.is_empty() || pattern.starts_with("[*]") {
        return None;
    }
    let bgr = hex[0];
    let (r, g, b) = (bgr & 0xff, (bgr >> 8) & 0xff, (bgr >> 16) & 0xff);
    Some(HighlightRule {
        text: pattern.to_string(),
        color: format!("#{r:02x}{g:02x}{b:02x}"),
        regex: hex[1] & 1 == 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}S:\"Keyword List\"=\r\n\
D:\"Match Case\"=00000001\r\n\
D:\"Regex Line Mode\"=00000000\r\n\
S:\"List Name\"=Cisco Words\r\n\
Z:\"Keyword List V2\"=00000002\r\n \"[*]Good_Words\",0073c372,00000001\r\n \"old\",0073c372,00000001\r\n\
Z:\"Keyword List V3\"=00000004\r\n \"[*]Good_Words\",0073c372,00000001,00000001\r\n \
\"(?:yes|permit|\\[OK\\]),?\",0073c372,00000001,00000001\r\n \
\"a\\\"quoted\\\",b\",004241ac,00000001,00000001\r\n \
\"shutdown\",000000ff,00000000,00000001\r\n";

    #[test]
    fn reads_v3_entries_colors_and_flags() {
        let p = parse(SAMPLE, "file").unwrap();
        assert_eq!(p.name, "Cisco Words");
        assert!(p.match_case);
        assert_eq!(p.rules.len(), 3, "labels skipped, V3 preferred over V2");
        assert_eq!(p.rules[0].text, r"(?:yes|permit|\[OK\]),?");
        assert_eq!(p.rules[0].color, "#72c373", "COLORREF is BGR");
        assert!(p.rules[0].regex);
        assert_eq!(
            p.rules[1].text, r#"a\"quoted\",b"#,
            "quotes and commas inside the pattern"
        );
        assert_eq!(p.rules[1].color, "#ac4142");
        assert_eq!(p.rules[2].text, "shutdown");
        assert_eq!(p.rules[2].color, "#ff0000");
        assert!(!p.rules[2].regex);
    }

    #[test]
    fn falls_back_to_v2_old_lists_and_file_name() {
        let v2 = "Z:\"Keyword List V2\"=00000001\n \"err\",000000ff,00000000\n";
        let p = parse(v2, "Mine").unwrap();
        assert_eq!(p.name, "Mine");
        assert_eq!(p.rules[0].text, "err");

        let old = "S:\"Keyword List\"=down error\n";
        assert_eq!(parse(old, "Old").unwrap().rules.len(), 2);

        assert!(parse("hello", "x").is_err());
    }

    /// Lists in a real SecureCRT install, when there is one:
    /// `cargo test -p linecard-importers -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_this_computers_lists() {
        for path in default_dir().map(|d| list(&d)).unwrap_or_default() {
            let text = std::fs::read_to_string(&path).unwrap();
            let p = parse(&text, "?").unwrap();
            println!("{}: {} rules", p.name, p.rules.len());
            assert!(!p.rules.is_empty());
        }
    }
}
