//! The hand-written HTML scanner shared by the campus and academic parsers.
//!
//! The Rust package keeps no HTML parser dependency, so the markup rules live
//! here once and every caller reuses them. The scanner understands quoted
//! attributes, comments, void elements, raw-text elements, and nested elements
//! of the same tag; it does not implement HTML error recovery, implied end
//! tags, or foreign content.

pub(crate) fn has_class_token(attrs: &str, token: &str) -> bool {
    attr_value(attrs, "class").is_some_and(|class| {
        decode_entities(class)
            .split_whitespace()
            .any(|value| value == token)
    })
}

pub(crate) fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn joined_text(fragment: &str) -> String {
    text_nodes(fragment).concat()
}

pub(crate) fn text_nodes(fragment: &str) -> Vec<String> {
    let mut nodes = Vec::new();
    for token in Tokenizer::new(fragment) {
        if let Token::Text(text) = token {
            nodes.push(decode_entities(text));
        }
    }
    nodes
}

/// Python's `html_to_text` without the unescape step: every tag becomes a
/// space. Entities stay encoded and the surrounding whitespace is kept, so a
/// caller that needs the original ordering decodes first and trims afterwards.
pub(crate) fn strip_tags(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    for token in Tokenizer::new(html) {
        match token {
            Token::Text(text) => output.push_str(text),
            Token::Tag(_) | Token::Other => output.push(' '),
        }
    }
    output
}

pub(crate) fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        if let Some(end) = tail.find(';') {
            let entity = &tail[1..end];
            if let Some(character) = decode_entity(entity) {
                output.push(character);
                rest = &tail[end + 1..];
                continue;
            }
        }
        output.push('&');
        rest = &tail[1..];
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        hex if hex.starts_with("#x") || hex.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&hex[2..], 16).ok()?)
        }
        decimal if decimal.starts_with('#') => char::from_u32(decimal[1..].parse::<u32>().ok()?),
        _ => None,
    }
}

/// Detect `attribute="value"` / `attribute='value'` anywhere in the markup,
/// which is what the Python page-context regexes effectively check.
pub(crate) fn has_attribute_value(html: &str, names: &[&str], value: &str) -> bool {
    let bytes = html.as_bytes();
    for index in 0..bytes.len() {
        for name in names {
            if !starts_with_ignore_ascii_case(bytes, index, name.as_bytes()) {
                continue;
            }
            let mut cursor = index + name.len();
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'=' {
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || (bytes[cursor] != b'"' && bytes[cursor] != b'\'') {
                continue;
            }
            let quote = bytes[cursor];
            cursor += 1;
            if starts_with_ignore_ascii_case(bytes, cursor, value.as_bytes()) {
                let end = cursor + value.len();
                if end < bytes.len() && bytes[end] == quote {
                    return true;
                }
            }
        }
    }
    false
}

/// Compare `needle` against `bytes` at `index`, ignoring ASCII case.
pub(crate) fn starts_with_ignore_ascii_case(bytes: &[u8], index: usize, needle: &[u8]) -> bool {
    index + needle.len() <= bytes.len()
        && bytes[index..index + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

pub(crate) struct Element<'a> {
    pub(crate) attrs: &'a str,
    pub(crate) start: usize,
    pub(crate) content_start: usize,
    pub(crate) content_end: usize,
    pub(crate) end: usize,
}

/// Return the outermost elements named `name` that satisfy `accept`, in
/// document order. Descendants of a match are not reported separately.
pub(crate) fn elements<'a>(
    text: &'a str,
    name: &str,
    mut accept: impl FnMut(&str) -> bool,
) -> Vec<Element<'a>> {
    let mut found = Vec::new();
    let mut tokenizer = Tokenizer::new(text);
    while let Some(token) = tokenizer.next() {
        let Token::Tag(tag) = token else {
            continue;
        };
        if tag.closing || !tag.name.eq_ignore_ascii_case(name) || !accept(tag.attrs) {
            continue;
        }
        if tag.self_closing || is_void(tag.name) {
            found.push(Element {
                attrs: tag.attrs,
                start: tag.start,
                content_start: tag.end,
                content_end: tag.end,
                end: tag.end,
            });
            continue;
        }
        match matching_close(text, tag.end, tag.name) {
            Some((close_start, close_end)) => {
                found.push(Element {
                    attrs: tag.attrs,
                    start: tag.start,
                    content_start: tag.end,
                    content_end: close_start,
                    end: close_end,
                });
                tokenizer.seek(close_end);
            }
            None => {
                found.push(Element {
                    attrs: tag.attrs,
                    start: tag.start,
                    content_start: tag.end,
                    content_end: text.len(),
                    end: text.len(),
                });
                tokenizer.seek(text.len());
            }
        }
    }
    found
}

fn matching_close(text: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut tokenizer = Tokenizer::new(text);
    tokenizer.seek(from);
    for token in tokenizer {
        let Token::Tag(tag) = token else {
            continue;
        };
        if !tag.name.eq_ignore_ascii_case(name) {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Some((tag.start, tag.end));
            }
        } else if !tag.self_closing && !is_void(tag.name) {
            depth += 1;
        }
    }
    None
}

fn is_void(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_raw_text(name: &str) -> bool {
    name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style")
}

pub(crate) fn attr_value<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let bytes = attrs.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
            && bytes[index] != b'>'
        {
            index += 1;
        }
        let key = &attrs[key_start..index];
        if key.is_empty() {
            index += 1;
            continue;
        }
        let mut cursor = index;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            if key.eq_ignore_ascii_case(name) {
                return Some("");
            }
            index = cursor.max(index + 1);
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let (value, next) =
            if cursor < bytes.len() && (bytes[cursor] == b'"' || bytes[cursor] == b'\'') {
                let quote = bytes[cursor];
                let value_start = cursor + 1;
                let mut end = value_start;
                while end < bytes.len() && bytes[end] != quote {
                    end += 1;
                }
                (&attrs[value_start..end], (end + 1).min(bytes.len()))
            } else {
                let value_start = cursor;
                let mut end = cursor;
                while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                (&attrs[value_start..end], end)
            };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        index = next.max(index + 1);
    }
    None
}

enum Token<'a> {
    Text(&'a str),
    Tag(Tag<'a>),
    Other,
}

struct Tag<'a> {
    name: &'a str,
    attrs: &'a str,
    closing: bool,
    self_closing: bool,
    start: usize,
    end: usize,
}

struct Tokenizer<'a> {
    text: &'a str,
    cursor: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, cursor: 0 }
    }

    fn seek(&mut self, position: usize) {
        self.cursor = position;
    }

    fn skip_raw_text(&mut self, name: &str, from: usize) {
        let rest = &self.text[from..];
        let lower = rest.to_ascii_lowercase();
        let needle = format!("</{}", name.to_ascii_lowercase());
        match lower.find(&needle) {
            Some(relative) => {
                let close_start = from + relative;
                let end = self.text[close_start..]
                    .find('>')
                    .map(|index| close_start + index + 1)
                    .unwrap_or(self.text.len());
                self.cursor = end;
            }
            None => self.cursor = self.text.len(),
        }
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let bytes = self.text.as_bytes();
        if self.cursor >= bytes.len() {
            return None;
        }
        let Some(relative) = self.text[self.cursor..].find('<') else {
            let text = &self.text[self.cursor..];
            self.cursor = bytes.len();
            return Some(Token::Text(text));
        };
        let start = self.cursor + relative;
        if start > self.cursor {
            let text = &self.text[self.cursor..start];
            self.cursor = start;
            return Some(Token::Text(text));
        }

        let rest = &self.text[start..];
        let after = &rest[1..];
        if after.starts_with("!--") {
            self.cursor = rest
                .find("-->")
                .map(|index| start + index + 3)
                .unwrap_or(bytes.len());
            return Some(Token::Other);
        }
        if after.starts_with('!') || after.starts_with('?') {
            self.cursor = rest
                .find('>')
                .map(|index| start + index + 1)
                .unwrap_or(bytes.len());
            return Some(Token::Other);
        }

        let closing = after.starts_with('/');
        let name_start = 1 + usize::from(closing);
        if !matches!(
            rest.as_bytes().get(name_start),
            Some(byte) if byte.is_ascii_alphabetic()
        ) {
            self.cursor = start + 1;
            return Some(Token::Text("<"));
        }
        let mut name_end = name_start;
        while name_end < rest.len()
            && !rest.as_bytes()[name_end].is_ascii_whitespace()
            && rest.as_bytes()[name_end] != b'>'
            && rest.as_bytes()[name_end] != b'/'
        {
            name_end += 1;
        }
        let name = &rest[name_start..name_end];

        let mut quote: Option<u8> = None;
        let mut index = name_end;
        while index < rest.len() {
            let byte = rest.as_bytes()[index];
            match quote {
                Some(open) if byte == open => quote = None,
                Some(_) => {}
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                None if byte == b'>' => break,
                None => {}
            }
            index += 1;
        }
        let attrs = &rest[name_end..index];
        let end = (start + index + 1).min(bytes.len());
        let self_closing = attrs.trim_end().ends_with('/');
        self.cursor = end;
        if !closing && !self_closing && is_raw_text(name) {
            self.skip_raw_text(name, end);
        }
        Some(Token::Tag(Tag {
            name,
            attrs,
            closing,
            self_closing,
            start,
            end,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_decode_named_numeric_and_unknown_forms() {
        assert_eq!(decode_entities("A &amp; B&nbsp;C"), "A & B\u{a0}C");
        assert_eq!(decode_entities("&#20320;&#x597D;"), "你好");
        assert_eq!(decode_entities("AT&T &unknown; &#;"), "AT&T &unknown; &#;");
    }

    #[test]
    fn strip_tags_makes_one_space_per_tag_and_leaves_entities_encoded() {
        assert_eq!(strip_tags("<p>a<br>b</p>").trim(), "a b");
        assert_eq!(strip_tags("A &amp; B"), "A &amp; B");
        assert_eq!(
            strip_tags("<p>x</p><script>var a = \"<p>\";</script><p>y</p>").trim(),
            "x   y"
        );
    }

    #[test]
    fn text_helpers_decode_each_text_run_and_collapse_spaces() {
        assert_eq!(text_nodes("<p>a&amp;b</p><p>c</p>"), ["a&b", "c"]);
        assert_eq!(joined_text("<p>a&amp;b</p><i>c</i>"), "a&bc");
        assert_eq!(joined_text("<p>x</p><script>var marker = 1;</script>"), "x");
        assert_eq!(collapse_whitespace("  a \n b "), "a b");
    }

    #[test]
    fn attr_value_reads_quoted_and_unquoted_values() {
        let attrs = r#" href="/a?x=1&y=2>b" title='q' data-x=1 disabled"#;
        assert_eq!(attr_value(attrs, "href"), Some("/a?x=1&y=2>b"));
        assert_eq!(attr_value(attrs, "title"), Some("q"));
        assert_eq!(attr_value(attrs, "data-x"), Some("1"));
        assert_eq!(attr_value(attrs, "disabled"), Some(""));
        assert_eq!(attr_value(attrs, "missing"), None);
    }

    #[test]
    fn has_class_token_matches_whole_decoded_tokens() {
        let attrs = r#" class="a newslist b""#;
        assert!(has_class_token(attrs, "newslist"));
        assert!(!has_class_token(attrs, "newslis"));
        assert!(!has_class_token(r#" id="newslist""#, "newslist"));
    }

    #[test]
    fn has_attribute_value_needs_a_complete_quoted_value() {
        let html = r#"<input TYPE="Password" name="execution">"#;
        assert!(has_attribute_value(html, &["name", "type"], "password"));
        assert!(has_attribute_value(html, &["name"], "execution"));
        assert!(!has_attribute_value(html, &["type"], "pass"));
        assert!(!has_attribute_value(html, &["id"], "execution"));
    }

    #[test]
    fn elements_reports_outermost_matches_and_skips_their_descendants() {
        let html = r#"<div class="a"><div class="a">inner</div></div><div class="b">b</div>"#;
        let found = elements(html, "div", |attrs| has_class_token(attrs, "a"));
        assert_eq!(found.len(), 1);
        assert_eq!(
            &html[found[0].content_start..found[0].content_end],
            r#"<div class="a">inner</div>"#
        );
        assert_eq!(
            &html[found[0].start..found[0].end],
            r#"<div class="a"><div class="a">inner</div></div>"#
        );
    }
}
