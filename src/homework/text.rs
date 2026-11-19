//! Small, dependency-free text helpers shared by the read-only homework views.
//!
//! They mirror the Python helpers closely enough for field parity without
//! pulling HTML or regex crates into this migration slice.

/// Strip tags, unescape common entities, and trim, like Python `html_to_text`.
pub(super) fn html_to_text(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut stripped = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '<' {
            // A tag is the run through the next `>`. An unmatched `<` is kept
            // as text rather than silently dropping the rest of the string.
            let mut candidate = String::new();
            let mut consumed = false;
            for next in chars.by_ref() {
                if next == '>' {
                    consumed = true;
                    break;
                }
                candidate.push(next);
            }
            if consumed {
                stripped.push(' ');
            } else {
                stripped.push('<');
                stripped.push_str(&candidate);
            }
        } else {
            stripped.push(character);
        }
    }
    unescape_entities(&stripped).trim().to_owned()
}

/// Collapse whitespace runs and strip, like Python `" ".join(value.split())`.
pub(super) fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One-line text for best-effort diagnostics.
pub(super) fn one_line(value: &str) -> String {
    normalize_text(value)
}

/// Replace every known secret before callers truncate the result.
pub(super) fn redact(mut value: String, secrets: &[&str]) -> String {
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        value = value.replace(secret, "[REDACTED]");
    }
    value
}

pub(super) fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn unescape_entities(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        let Some(end) = after.find(';') else {
            output.push('&');
            rest = after;
            continue;
        };
        // Avoid treating an arbitrarily long span as an entity.
        if end > 32 {
            output.push('&');
            rest = after;
            continue;
        }
        let entity = &after[..end];
        if let Some(decoded) = decode_entity(entity) {
            output.push(decoded);
            rest = &after[end + 1..];
        } else {
            output.push('&');
            rest = after;
        }
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    if let Some(number) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(number, 16)
            .ok()
            .and_then(char::from_u32);
    }
    if let Some(number) = entity.strip_prefix('#') {
        return number.parse::<u32>().ok().and_then(char::from_u32);
    }
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        "copy" => Some('\u{a9}'),
        "reg" => Some('\u{ae}'),
        "hellip" => Some('\u{2026}'),
        "mdash" => Some('\u{2014}'),
        "ndash" => Some('\u{2013}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_tags_and_unescapes_entities() {
        assert_eq!(
            normalize_text(&html_to_text("<p>a &amp; b</p><br>c&nbsp;d")),
            "a & b c d"
        );
        assert_eq!(html_to_text("<p>你好&#x4E16;界</p>"), "你好世界");
        assert_eq!(html_to_text("a < b"), "a < b");
    }

    #[test]
    fn normalization_collapses_whitespace_without_trimming_raw_html() {
        assert_eq!(normalize_text(" a \n\t b  c "), "a b c");
        assert_eq!(html_to_text("<p>a    b</p>"), "a    b");
    }

    #[test]
    fn redaction_happens_before_the_caller_truncates() {
        let text = redact(
            "failure synthetic-access-token tail".into(),
            &["synthetic-access-token"],
        );
        assert_eq!(text, "failure [REDACTED] tail");
        assert_eq!(truncate_chars("你好世界", 2), "你好");
    }
}
