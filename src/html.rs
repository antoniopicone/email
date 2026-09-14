//! HTML handling for message bodies.
//!
//! Mail is hostile input. Before anything reaches the WebView we strip scripts,
//! event handlers, embedded objects and remote-loading tags, and we render the
//! result with JavaScript disabled and remote content blocked by default.

use std::collections::{HashMap, HashSet};

use ammonia::Builder;

use crate::config::MessageAppearance;

/// Sanitise a message's HTML body.
///
/// Remote images are neutralised by moving `src` to `data-remote-src`, so the
/// UI can offer a "load remote content" action without leaking a read receipt
/// the moment the message is opened. [`allow_remote_images`] restores them
/// once the user (or their preference) allows it.
pub fn sanitize(html: &str) -> String {
    let mut tags = default_tags();
    tags.insert("img");

    let mut builder = Builder::default();
    builder
        .tags(tags)
        .rm_tags(["script", "style", "iframe", "object", "embed", "form", "input", "base"])
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .add_generic_attributes(["style", "align", "valign", "bgcolor", "width", "height"])
        .add_tag_attributes("img", ["src", "alt"]);

    let cleaned = builder.clean(html).to_string();
    defer_remote_images(&cleaned)
}

/// Rewrite `<img src=...>` into an inert placeholder: the network-facing
/// `src` becomes `data-remote-src`, so parsing the document can never trigger
/// a fetch on its own.
fn defer_remote_images(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<img ") {
        out.push_str(&rest[..start]);
        out.push_str("<img data-blocked=\"1\" ");
        rest = &rest[start + "<img ".len()..];

        let Some(end) = rest.find('>') else {
            out.push_str(rest);
            return out;
        };
        let (attrs, after) = rest.split_at(end);
        out.push_str(&attrs.replacen("src=", "data-remote-src=", 1));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Restore the `src` neutralised by [`sanitize`], so the images actually
/// load — called at render time once remote content is allowed.
pub fn allow_remote_images(html: &str) -> String {
    html.replace("data-blocked=\"1\" data-remote-src=", "src=")
}

fn default_tags() -> HashSet<&'static str> {
    [
        "a", "abbr", "b", "blockquote", "br", "caption", "cite", "code", "col", "colgroup",
        "dd", "del", "div", "dl", "dt", "em", "figcaption", "figure", "h1", "h2", "h3", "h4",
        "h5", "h6", "hr", "i", "ins", "li", "ol", "p", "pre", "q", "small", "span", "strong",
        "sub", "sup", "table", "tbody", "td", "tfoot", "th", "thead", "tr", "u", "ul",
    ]
    .into_iter()
    .collect()
}

/// Collapse an HTML fragment to readable plain text, for list previews and for
/// messages we choose not to render as HTML.
pub fn to_plain_text(html: &str) -> String {
    if !html.contains('<') {
        return decode_entities(html);
    }

    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip_depth = 0usize;
    let mut tag_name = String::new();

    let bytes: Vec<char> = html.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '<' {
            in_tag = true;
            tag_name.clear();
            let mut j = i + 1;
            let closing = bytes.get(j) == Some(&'/');
            if closing {
                j += 1;
            }
            while j < bytes.len() && (bytes[j].is_alphanumeric()) {
                tag_name.push(bytes[j].to_ascii_lowercase());
                j += 1;
            }
            // Drop the contents of tags that never carry readable text.
            if matches!(tag_name.as_str(), "script" | "style" | "head" | "title") {
                if closing {
                    skip_depth = skip_depth.saturating_sub(1);
                } else {
                    skip_depth += 1;
                }
            }
            // Block-level tags become line breaks.
            if matches!(
                tag_name.as_str(),
                "p" | "div" | "br" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if c == '>' {
            in_tag = false;
            i += 1;
            continue;
        }
        if !in_tag && skip_depth == 0 {
            out.push(c);
        }
        i += 1;
    }

    let decoded = decode_entities(&out);
    // Squash the runs of blank lines the tag-to-newline mapping creates.
    let mut result = String::with_capacity(decoded.len());
    let mut blank_run = 0;
    for line in decoded.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        result.push_str(line.trim_end());
        result.push('\n');
    }
    result.trim().to_string()
}

fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let named: HashMap<&str, &str> = HashMap::from([
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", " "),
        ("mdash", "—"),
        ("ndash", "–"),
        ("hellip", "…"),
        ("rsquo", "’"),
        ("lsquo", "‘"),
        ("ldquo", "“"),
        ("rdquo", "”"),
        ("eacute", "é"),
        ("egrave", "è"),
        ("agrave", "à"),
        ("ograve", "ò"),
        ("ugrave", "ù"),
        ("igrave", "ì"),
        ("euro", "€"),
        ("copy", "©"),
        ("reg", "®"),
        ("trade", "™"),
    ]);

    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' {
            if let Some(end) = chars[i..].iter().position(|c| *c == ';').filter(|p| *p <= 10) {
                let entity: String = chars[i + 1..i + end].iter().collect();
                if let Some(stripped) = entity.strip_prefix('#') {
                    let code = if let Some(hex) = stripped.strip_prefix(['x', 'X']) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        stripped.parse::<u32>().ok()
                    };
                    if let Some(ch) = code.and_then(char::from_u32) {
                        out.push(ch);
                        i += end + 1;
                        continue;
                    }
                } else if let Some(replacement) = named.get(entity.to_ascii_lowercase().as_str()) {
                    out.push_str(replacement);
                    i += end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Escape text for safe interpolation into our own HTML wrapper.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wrap a message body in a document styled to match the current GTK theme, so
/// the WebView does not flash white inside a dark window.
///
/// `appearance` decides how much of that styling actually reaches the
/// message: [`MessageAppearance::AdaptText`] forces only the text colour,
/// [`MessageAppearance::AdaptBackground`] forces only the background, and
/// [`MessageAppearance::AcceptSenderFormat`] forces neither and always uses
/// the light palette, leaving the message exactly as the sender authored it.
pub fn wrap_document(body_html: &str, dark: bool, appearance: MessageAppearance) -> String {
    let effective_dark = dark && appearance != MessageAppearance::AcceptSenderFormat;
    let (bg, fg, quote, link, border) = if effective_dark {
        ("#1d1d20", "#f2f2f5", "#9a9aa5", "#7cb7ff", "#3a3a40")
    } else {
        ("#ffffff", "#1c1c1e", "#6b6b70", "#0a68d8", "#e2e2e6")
    };

    let base_rule = match appearance {
        MessageAppearance::AdaptText => format!("color: {fg};"),
        MessageAppearance::AdaptBackground => format!("background: {bg};"),
        MessageAppearance::AcceptSenderFormat => String::new(),
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root {{ color-scheme: {scheme}; }}
  html, body {{
    margin: 0;
    padding: 0 22px 28px 22px;
    {base_rule}
    font-family: 'Cantarell', 'Inter', system-ui, -apple-system, sans-serif;
    font-size: 14.5px;
    line-height: 1.55;
    word-wrap: break-word;
    overflow-wrap: break-word;
  }}
  a {{ color: {link}; }}
  blockquote {{
    margin: 0.6em 0;
    padding-left: 12px;
    border-left: 3px solid {border};
    color: {quote};
  }}
  pre, code {{
    font-family: 'Source Code Pro', ui-monospace, monospace;
    font-size: 13px;
    white-space: pre-wrap;
  }}
  pre {{
    background: rgba(127,127,127,0.12);
    padding: 10px 12px;
    border-radius: 8px;
  }}
  table {{ border-collapse: collapse; max-width: 100%; }}
  td, th {{ padding: 4px 8px; }}
  img {{ max-width: 100%; height: auto; }}
  img[data-blocked] {{
    display: inline-block;
    min-width: 42px;
    min-height: 42px;
    background: rgba(127,127,127,0.14);
    border: 1px dashed {border};
    border-radius: 6px;
  }}
  hr {{ border: none; border-top: 1px solid {border}; }}
  .plain {{ white-space: pre-wrap; font-family: inherit; }}
</style>
</head>
<body>{body_html}</body>
</html>"#,
        scheme = if effective_dark { "dark" } else { "light" },
    )
}

/// Render a plain-text body as an HTML document, linkifying bare URLs.
pub fn plain_text_document(text: &str, dark: bool, appearance: MessageAppearance) -> String {
    let escaped = escape(text);
    let linked = linkify(&escaped);
    wrap_document(&format!("<div class=\"plain\">{linked}</div>"), dark, appearance)
}

/// Turn bare `http(s)://` runs into anchors. Input must already be escaped.
fn linkify(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(idx) = rest.find("http") {
        let (before, tail) = rest.split_at(idx);
        if !(tail.starts_with("http://") || tail.starts_with("https://")) {
            out.push_str(before);
            out.push_str(&tail[..4]);
            rest = &tail[4..];
            continue;
        }
        out.push_str(before);
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == '"')
            .unwrap_or(tail.len());
        let (url, remainder) = tail.split_at(end);
        let trimmed = url.trim_end_matches(['.', ',', ')', ';', ':']);
        out.push_str(&format!("<a href=\"{trimmed}\">{trimmed}</a>"));
        out.push_str(&url[trimmed.len()..]);
        rest = remainder;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_and_handlers() {
        let dirty = r#"<p onclick="steal()">ciao</p><script>alert(1)</script>"#;
        let clean = sanitize(dirty);
        assert!(!clean.contains("script"));
        assert!(!clean.contains("onclick"));
        assert!(clean.contains("ciao"));
    }

    #[test]
    fn neutralises_and_restores_remote_images() {
        let dirty = r#"<img src="https://evil.example/pixel.gif" alt="x">"#;
        let clean = sanitize(dirty);
        assert!(!clean.contains(" src=\"https://evil.example"), "a live src slipped through: {clean}");
        assert!(clean.contains("data-remote-src=\"https://evil.example/pixel.gif\""));

        let restored = allow_remote_images(&clean);
        assert!(restored.contains("src=\"https://evil.example/pixel.gif\""));
        assert!(!restored.contains("data-remote-src="));
    }

    #[test]
    fn plain_text_drops_markup() {
        let text = to_plain_text("<div>uno</div><p>due &amp; tre</p>");
        assert!(text.contains("uno"));
        assert!(text.contains("due & tre"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn decodes_numeric_entities() {
        assert_eq!(decode_entities("caff&#232;"), "caffè");
        assert_eq!(decode_entities("a &lt; b"), "a < b");
    }

    #[test]
    fn linkifies_urls() {
        let out = linkify("vedi https://example.com/x ok");
        assert!(out.contains(r#"<a href="https://example.com/x">"#));
    }
}
