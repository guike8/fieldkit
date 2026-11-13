//! Conservative, byte-native extraction of historical tracking identifier literals.
//!
//! This is not a browser, HTML5 parser, or JavaScript interpreter. Only whole,
//! unescaped single/double-quoted strings in JavaScript script blocks and whole
//! `data-ad-client`/`ad-client` attribute values are considered. HTML comments,
//! common example/inert containers, JavaScript comments, templates, and non-JS
//! script blocks are skipped. A template or an ambiguous non-comment slash
//! stops scanning that script rather than guessing at JavaScript grammar.
//! Malformed markup and unfamiliar JS syntax can be missed; external scripts,
//! entity/escape decoding, concatenation, runtime
//! values, and account-prefix inference are deliberately unsupported.
//!
//! Even a retained script literal can be a sample or unused code. The only claim
//! is `contains_tracking_literal`, never execution, ownership, or site control.
//! All offsets are half-open ranges into the ORIGINAL input bytes.

use serde_json::{Value, json};
use std::ops::Range;

const SCOPE: &str = "contains_tracking_literal";
const COVERAGE: &str = "Conservative static HTML/JavaScript literal scan, not a complete HTML5 or JavaScript parser; excludes common comments/example containers; templates or ambiguous non-comment slashes stop that script scan; no decoding, execution, external scripts, dynamic evaluation, account-prefix inference, ownership or control conclusions";

/// Return source-ordered occurrences (including duplicates), bounded by `maximum`.
///
/// `count` is retained occurrences; `total_count` counts all eligible occurrences,
/// including duplicates; `truncated` means `total_count > count`. Canonical UA
/// values retain the full property suffix. Canonical AdSense values omit `ca-`.
/// An empty or zero-limit result has the same shape as every other result.
pub fn extract(body: &[u8], maximum: usize) -> Value {
    let mut output = Output {
        body,
        maximum,
        identifiers: Vec::new(),
        total_count: 0,
    };
    let mut cursor = 0;
    let mut excluded: Vec<&[u8]> = Vec::new();
    while cursor < body.len() {
        if body[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        if body[cursor..].starts_with(b"<!--") {
            cursor = find_bytes(body, cursor + 4, b"-->").map_or(body.len(), |end| end + 3);
            continue;
        }
        // Declarations/processing instructions are not elements or code.
        if matches!(body.get(cursor + 1), Some(b'!' | b'?')) {
            cursor = tag_end(body, cursor + 2).unwrap_or(body.len());
            continue;
        }
        let Some(tag) = tag(body, cursor) else {
            // An unterminated real tag may contain '<script>' inside an open
            // quoted attribute. Do not restart a scan in that attribute text.
            let possible_name = if body.get(cursor + 1) == Some(&b'/') {
                body.get(cursor + 2)
            } else {
                body.get(cursor + 1)
            };
            if possible_name.is_some_and(u8::is_ascii_alphabetic) {
                break;
            }
            cursor += 1;
            continue;
        };
        cursor = tag.end;
        if tag.closing {
            if let Some(index) = excluded
                .iter()
                .rposition(|name| name.eq_ignore_ascii_case(tag.name))
            {
                excluded.truncate(index);
            }
            continue;
        }
        if tag.name.eq_ignore_ascii_case(b"plaintext") {
            break;
        }
        if is_example_container(tag.name) {
            // HTML non-void elements are not actually self-closing, even with />.
            excluded.push(tag.name);
        }
        if excluded.is_empty() {
            for attr in &tag.attributes {
                if (attr.name.eq_ignore_ascii_case(b"data-ad-client")
                    || attr.name.eq_ignore_ascii_case(b"ad-client"))
                    && let Some(range) = &attr.value
                {
                    output.record(range.clone(), "ad_client_attribute", true);
                }
            }
        }
        if is_raw_element(tag.name) {
            let (content_end, element_end) = raw_end(body, cursor, tag.name);
            if excluded.is_empty()
                && tag.name.eq_ignore_ascii_case(b"script")
                && javascript_type(body, &tag.attributes)
            {
                script_literals(body, cursor..content_end, &mut output);
            }
            cursor = element_end;
        }
    }
    let count = output.identifiers.len();
    json!({
        "identifiers": output.identifiers,
        "count": count,
        "total_count": output.total_count,
        "truncated": output.total_count > count,
        "maximum": maximum,
        "scope": SCOPE,
        "scripts_executed": false,
        "coverage": COVERAGE
    })
}

struct Output<'a> {
    body: &'a [u8],
    maximum: usize,
    identifiers: Vec<Value>,
    total_count: usize,
}

impl Output<'_> {
    fn record(&mut self, range: Range<usize>, context: &str, publisher_only: bool) {
        let raw = &self.body[range.clone()];
        let Some((kind, value, identifier_scope)) = identifier(raw) else {
            return;
        };
        if publisher_only && kind != "google_adsense_publisher" {
            return;
        }
        self.total_count = self.total_count.saturating_add(1);
        if self.identifiers.len() < self.maximum {
            // identifier() accepts ASCII only, so this never re-encodes source bytes.
            let raw_value = std::str::from_utf8(raw).expect("ASCII tracking identifier");
            self.identifiers.push(json!({
                "raw_value": raw_value,
                "value": value,
                "kind": kind,
                "byte_start": range.start,
                "byte_end": range.end,
                "scope": SCOPE,
                "context": context,
                "identifier_scope": identifier_scope
            }));
        }
    }
}

fn identifier(raw: &[u8]) -> Option<(&'static str, String, &'static str)> {
    if raw.len() > 3 && raw[..3].eq_ignore_ascii_case(b"UA-") {
        let rest = &raw[3..];
        let dash = rest.iter().position(|b| *b == b'-')?;
        let (account, property) = (&rest[..dash], &rest[dash + 1..]);
        if !account.is_empty()
            && !property.is_empty()
            && account.iter().all(u8::is_ascii_digit)
            && property.iter().all(u8::is_ascii_digit)
        {
            return Some((
                "google_analytics_ua_property",
                format!("UA-{}", std::str::from_utf8(rest).ok()?),
                "exact_property",
            ));
        }
        return None;
    }
    let publisher = if raw.len() >= 3 && raw[..3].eq_ignore_ascii_case(b"ca-") {
        &raw[3..]
    } else {
        raw
    };
    if publisher.len() == 20
        && publisher[..4].eq_ignore_ascii_case(b"pub-")
        && publisher[4..].iter().all(u8::is_ascii_digit)
    {
        return Some((
            "google_adsense_publisher",
            format!("pub-{}", std::str::from_utf8(&publisher[4..]).ok()?),
            "exact_publisher",
        ));
    }
    None
}

struct Attribute<'a> {
    name: &'a [u8],
    value: Option<Range<usize>>,
}

struct Tag<'a> {
    name: &'a [u8],
    closing: bool,
    end: usize,
    attributes: Vec<Attribute<'a>>,
}

// End is exclusive; quoted '>' bytes cannot end a tag.
fn tag_end(body: &[u8], mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(&byte) = body.get(cursor) {
        match quote {
            Some(q) if byte == q => quote = None,
            None if matches!(byte, b'\'' | b'"') => quote = Some(byte),
            None if byte == b'>' => return Some(cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn tag(body: &[u8], start: usize) -> Option<Tag<'_>> {
    let mut cursor = start + 1;
    let closing = body.get(cursor) == Some(&b'/');
    cursor += usize::from(closing);
    if !body.get(cursor)?.is_ascii_alphabetic() {
        return None;
    }
    let name_start = cursor;
    while body
        .get(cursor)
        .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'/' | b'>'))
    {
        cursor += 1;
    }
    let name = &body[name_start..cursor];
    let end = tag_end(body, cursor)?;
    let mut attributes = Vec::new();
    while cursor < end - 1 {
        if body[cursor].is_ascii_whitespace() || body[cursor] == b'/' {
            cursor += 1;
            continue;
        }
        let attr_start = cursor;
        while cursor < end - 1
            && !body[cursor].is_ascii_whitespace()
            && !matches!(body[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let attr_name = &body[attr_start..cursor];
        while cursor < end - 1 && body[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let mut value = None;
        if body[cursor] == b'=' {
            cursor += 1;
            while cursor < end - 1 && body[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if matches!(body[cursor], b'\'' | b'"') {
                let quote = body[cursor];
                cursor += 1;
                let value_start = cursor;
                while cursor < end - 1 && body[cursor] != quote {
                    cursor += 1;
                }
                value = Some(value_start..cursor);
                cursor += 1;
            } else {
                let value_start = cursor;
                while cursor < end - 1 && !body[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                value = Some(value_start..cursor);
            }
        }
        if !attr_name.is_empty() {
            attributes.push(Attribute {
                name: attr_name,
                value,
            });
        } else if cursor < end - 1 {
            cursor += 1;
        }
    }
    Some(Tag {
        name,
        closing,
        end,
        attributes,
    })
}

fn is_example_container(name: &[u8]) -> bool {
    [
        b"pre".as_slice(),
        b"code",
        b"samp",
        b"kbd",
        b"template",
        b"noscript",
        b"listing",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn is_raw_element(name: &[u8]) -> bool {
    [
        b"script".as_slice(),
        b"style",
        b"textarea",
        b"title",
        b"xmp",
        b"iframe",
        b"noembed",
        b"noframes",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn javascript_type(body: &[u8], attributes: &[Attribute<'_>]) -> bool {
    let Some(attr) = attributes
        .iter()
        .find(|attr| attr.name.eq_ignore_ascii_case(b"type"))
    else {
        return true;
    };
    let Some(range) = &attr.value else {
        return true;
    };
    let value = &body[range.clone()];
    let start = value
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    let value = &value[start..end];
    value.is_empty()
        || [
            b"module".as_slice(),
            b"text/javascript",
            b"application/javascript",
            b"text/ecmascript",
            b"application/ecmascript",
        ]
        .iter()
        .any(|kind| value.eq_ignore_ascii_case(kind))
}

fn find_bytes(body: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    body.get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

// Script/raw-text end tags are recognized even inside JS quotes, like HTML.
fn raw_end(body: &[u8], mut cursor: usize, name: &[u8]) -> (usize, usize) {
    while let Some(start) = find_bytes(body, cursor, b"</") {
        let name_start = start + 2;
        let name_end = name_start + name.len();
        if body
            .get(name_start..name_end)
            .is_some_and(|s| s.eq_ignore_ascii_case(name))
            && body
                .get(name_end)
                .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
        {
            return (start, tag_end(body, name_end).unwrap_or(body.len()));
        }
        cursor = name_start;
    }
    (body.len(), body.len())
}

fn script_literals(body: &[u8], range: Range<usize>, output: &mut Output<'_>) {
    let mut cursor = range.start;
    while cursor < range.end {
        let rest = &body[cursor..range.end];
        if rest.starts_with(b"//") || rest.starts_with(b"<!--") || rest.starts_with(b"-->") {
            cursor = rest
                .iter()
                .position(|b| matches!(b, b'\n' | b'\r'))
                .map_or(range.end, |offset| cursor + offset + 1);
            continue;
        }
        if rest.starts_with(b"/*") {
            cursor =
                find_bytes(&body[..range.end], cursor + 2, b"*/").map_or(range.end, |end| end + 2);
            continue;
        }
        let quote = body[cursor];
        // Distinguishing regex literals from division and nested templates needs
        // a JS parser. Fail closed for the remainder of this script instead of
        // treating quote bytes inside either construct as string literals.
        if matches!(quote, b'/' | b'`') {
            break;
        }
        if !matches!(quote, b'\'' | b'"') {
            cursor += 1;
            continue;
        }
        cursor += 1;
        let start = cursor;
        let mut escaped = false;
        while cursor < range.end && body[cursor] != quote {
            if body[cursor] == b'\\' {
                escaped = true;
                cursor = (cursor + 2).min(range.end);
            } else {
                cursor += 1;
            }
        }
        if cursor == range.end {
            break;
        }
        // Template interpolation, escape sequences and even obvious concatenation
        // are intentionally not interpreted. Only the whole original literal fits.
        let previous = body[range.start..start - 1]
            .iter()
            .rfind(|b| !b.is_ascii_whitespace());
        let following = body[cursor + 1..range.end]
            .iter()
            .find(|b| !b.is_ascii_whitespace());
        if quote != b'`' && !escaped && previous != Some(&b'+') && following != Some(&b'+') {
            output.record(start..cursor, "script_literal", false);
        }
        cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_identifiers_are_exact_properties_not_account_prefixes() {
        let found = extract(
            br#"<script>ga('create', 'UA-101199457-4', 'auto');
            ga('create', 'UA-101199457-5', 'auto');
            google_ad_client = 'ca-pub-8264869885899896';</script>"#,
            10,
        );
        assert_eq!(found["count"], 3);
        assert_eq!(found["total_count"], 3);
        assert_eq!(found["truncated"], false);
        assert_eq!(found["identifiers"][0]["value"], "UA-101199457-4");
        assert_eq!(found["identifiers"][1]["value"], "UA-101199457-5");
        assert_eq!(
            found["identifiers"][0]["identifier_scope"],
            "exact_property"
        );
        assert_eq!(
            found["identifiers"][2]["raw_value"],
            "ca-pub-8264869885899896"
        );
        assert_eq!(found["identifiers"][2]["value"], "pub-8264869885899896");
        assert_eq!(found["identifiers"][2]["kind"], "google_adsense_publisher");
        assert_eq!(found["scope"], SCOPE);
        assert_eq!(found["scripts_executed"], false);
    }

    #[test]
    fn source_order_duplicates_case_and_limits_are_explicit() {
        let body = br#"<ins DATA-AD-CLIENT='Ca-PuB-8264869885899896'></ins>
            <ScRiPt TYPE='Text/JavaScript'>const a='uA-101199457-4';
            const b='UA-101199457-4'; const c='PUB-8264869885899896';</sCrIpT>"#;
        let found = extract(body, 3);
        assert_eq!(found["count"], 3);
        assert_eq!(found["total_count"], 4);
        assert_eq!(found["truncated"], true);
        assert_eq!(found["identifiers"][0]["value"], "pub-8264869885899896");
        assert_eq!(found["identifiers"][1]["raw_value"], "uA-101199457-4");
        assert_eq!(
            found["identifiers"][1]["value"],
            found["identifiers"][2]["value"]
        );
        let zero = extract(body, 0);
        assert_eq!(zero["identifiers"], json!([]));
        assert_eq!(zero["count"], 0);
        assert_eq!(zero["total_count"], 4);
        assert_eq!(zero["truncated"], true);
        assert_eq!(extract(b"", 0)["truncated"], false);
        assert_eq!(found, extract(body, 3));
    }

    #[test]
    fn rejects_prose_comments_examples_and_inert_markup() {
        let body = br#"<article>Examples: 'UA-101199457-4', ca-pub-8264869885899896.</article>
            <!-- <script>'UA-101199457-4'</script><ins data-ad-client='pub-8264869885899896'> -->
            <pre><script>'UA-101199457-4'</script><ins data-ad-client='pub-8264869885899896'></pre>
            <code><script>'UA-101199457-4'</script></code>
            <template><script>'UA-101199457-4'</script></template>
            <noscript><ins data-ad-client='pub-8264869885899896'></noscript>
            <textarea><script>'UA-101199457-4'</script></textarea>
            <script type='text/plain'>'UA-101199457-4'</script>
            <script type='application/ld+json'>{"example":"UA-101199457-4"}</script>
            <script>// 'UA-101199457-4'
            /* 'pub-8264869885899896' */
            <!-- 'UA-101199457-4'
            --> 'UA-101199457-4'
            </script>"#;
        assert_eq!(extract(body, 20)["count"], 0);
        assert_eq!(
            extract(br#"<pre/><script>'UA-101199457-4'</script>"#, 20)["count"],
            0
        );
    }

    #[test]
    fn rejects_invalid_long_tokens_escapes_and_dynamic_values() {
        let body = br#"<script>
            'UA-101199457'; 'UA--4'; 'UA-101199457-';
            'xUA-101199457-4'; 'UA-101199457-4x'; 'UA-101199457-4-5';
            'pub-826486988589989'; 'pub-82648698858998960';
            'xpub-8264869885899896'; 'ca-pub-8264869885899896x';
            'UA-101199457-\x34'; 'UA-101199457-' + '4';
            'UA-101199457-4' + suffix; prefix + 'UA-101199457-4';
            `UA-101199457-4`; `UA-${account}-4`;
            </script>
            <ins data-ad-client='UA-101199457-4'></ins>
            <ins data-ad-client='ca-pub-8264869885899896x'></ins>
            <ins data-ad-client='ca-pub-826486988589989&#54;'></ins>"#;
        assert_eq!(extract(body, 50)["count"], 0);
    }

    #[test]
    fn ambiguous_js_and_malformed_markup_are_not_reinterpreted_as_literals() {
        let body = br#"<script>const pattern = /'UA-101199457-4'/;</script>
            <script>const template = `${`nested` + 'UA-101199457-4'}`;</script>
            <div title="<script>'UA-101199457-4'</script>" data-example='x'>
            <a title="unterminated <script>'UA-101199457-4'</script>"#;
        assert_eq!(extract(body, 10)["count"], 0);
        // A false raw end-tag prefix must not expose its subsequent source as HTML.
        assert_eq!(
            extract(
                br#"<style></stylex><script>'UA-101199457-4'</script></style>"#,
                10
            )["count"],
            0
        );
    }

    #[test]
    fn offsets_bind_original_non_utf8_and_unicode_bytes() {
        let mut body = vec![0xff, 0xfe];
        body.extend_from_slice("雪🦀<script>const a='uA-101199457-4';</script>".as_bytes());
        body.push(0x80);
        body.extend_from_slice(br#"<ins data-ad-client="ca-pub-8264869885899896"></ins>"#);
        let found = extract(&body, 5);
        assert_eq!(found["count"], 2);
        for occurrence in found["identifiers"].as_array().unwrap() {
            let start = occurrence["byte_start"].as_u64().unwrap() as usize;
            let end = occurrence["byte_end"].as_u64().unwrap() as usize;
            assert_eq!(
                &body[start..end],
                occurrence["raw_value"].as_str().unwrap().as_bytes()
            );
        }
        assert_eq!(found["identifiers"][0]["byte_start"], 26);
    }

    #[test]
    fn scripts_remain_unexecuted_code_text_not_site_ownership() {
        let found = extract(
            br#"<script>
            throw new Error('must never execute');
            fetch('https://example.invalid/');
            const unused_sample = 'UA-101199457-4';
            </script>"#,
            10,
        );
        assert_eq!(found["count"], 1);
        assert_eq!(
            found["identifiers"][0]["scope"],
            "contains_tracking_literal"
        );
        assert_eq!(found["scripts_executed"], false);
    }
}
