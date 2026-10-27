//! External resource references in downloaded SVG images (#229).
//!
//! GPUI draws SVG with the stock usvg resolver, which reads local files
//! named by `<image href="/path">` (or `file://…`). A chart icon or a README
//! picture comes from a third-party host, so it must not make Kubach read
//! arbitrary local files (or hang on `/dev/zero` or a FIFO). Such an SVG is
//! refused before it is cached and before a cached file is handed to the UI.
//!
//! Refused: any `href` / `xlink:href` (`<image>`, `<use>`, `<feImage>`,
//! anything) that is neither an internal `#id` nor a `data:` URL, CSS
//! `url(…)` to anything else in `<style>` or in an attribute, CSS `@import`
//! and `<?xml-stylesheet?>`. Links (`<a href>`) are not resources and stay.
//! Nested `data:` SVG images are checked by [`super::dimensions`], which
//! calls [`check`] on them while parsing.

use super::ImageError;

/// Fails when the SVG in `bytes` refers to a resource outside itself.
/// The XML is parsed like usvg does (DTD allowed, entities expanded), so a
/// reference hidden behind an entity is seen too.
pub fn check(bytes: &[u8]) -> Result<(), ImageError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ImageError::NotAnImage)?;
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, options)
        .map_err(|_| ImageError::NotAnImage)?;
    for node in doc.descendants() {
        if let Some(pi) = node.pi()
            && pi.target.eq_ignore_ascii_case("xml-stylesheet")
        {
            return Err(external(pi.value.unwrap_or_default()));
        }
        if node.is_text()
            && node
                .parent_element()
                .is_some_and(|parent| parent.tag_name().name() == "style")
        {
            check_css(node.text().unwrap_or_default())?;
        }
        if !node.is_element() {
            continue;
        }
        let link = node.tag_name().name() == "a";
        for attribute in node.attributes() {
            let href = attribute.name() == "href";
            if href && link {
                continue;
            }
            let value = attribute.value();
            if href && !is_internal(value) {
                return Err(external(value));
            }
            check_css(value)?;
        }
    }
    Ok(())
}

/// An internal reference: empty, `#id` or a `data:` URL.
fn is_internal(reference: &str) -> bool {
    let reference = reference.trim();
    reference.is_empty()
        || reference.starts_with('#')
        || reference
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

/// Fails on `@import` and on `url(…)` to anything but `#id` or `data:` in
/// CSS (a `<style>` sheet or an attribute value). Comments and escapes are
/// undone first, so `u\72l(` or `@im/**/port` don't slip through.
fn check_css(css: &str) -> Result<(), ImageError> {
    if !css.contains(['(', '@', '\\']) {
        return Ok(());
    }
    let css = unescape(&strip_comments(css));
    let lower = css.to_ascii_lowercase();
    if lower.contains("@import") {
        return Err(external("@import"));
    }
    let mut from = 0;
    while let Some(ix) = lower[from..].find("url(") {
        let start = from + ix + 4;
        from = start;
        let target = css[start..].trim_start();
        let target = target.trim_start_matches(['"', '\'']);
        let end = target.find([')', '"', '\'']).unwrap_or(target.len());
        if !is_internal(&target[..end]) {
            return Err(external(&target[..end]));
        }
    }
    Ok(())
}

/// `css` without `/* … */` comments (an unclosed one runs to the end).
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(ix) = rest.find("/*") {
        out.push_str(&rest[..ix]);
        rest = match rest[ix + 2..].find("*/") {
            Some(end) => &rest[ix + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// `css` with CSS escapes undone: `\` and up to 6 hex digits (plus one
/// whitespace) is that code point, `\` and any other character is that
/// character.
fn unescape(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 {
            match chars.peek() {
                Some(h) if h.is_ascii_hexdigit() => {
                    hex.push(*h);
                    chars.next();
                }
                _ => break,
            }
        }
        if hex.is_empty() {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let code = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
    }
    out
}

fn external(reference: &str) -> ImageError {
    let reference = reference.trim();
    let shown: String = reference.chars().take(100).collect();
    ImageError::ExternalReference(shown)
}

#[cfg(test)]
mod tests {
    use super::{ImageError, check};

    fn svg(body: &str) -> Vec<u8> {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" \
             xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"16\" height=\"16\">{body}</svg>"
        )
        .into_bytes()
    }

    fn refused(body: &str) -> bool {
        matches!(check(&svg(body)), Err(ImageError::ExternalReference(_)))
    }

    #[test]
    fn local_files_are_refused() {
        assert!(refused(
            "<image href=\"/etc/passwd\" width=\"1\" height=\"1\"/>"
        ));
        assert!(refused("<image href=\"file:///etc/passwd\"/>"));
        assert!(refused("<image href=\"  FILE:///etc/hosts\"/>"));
        assert!(refused("<image xlink:href=\"/dev/zero\"/>"));
        assert!(refused("<image href=\"C:\\Windows\\win.ini\"/>"));
        assert!(refused("<image href=\"https://example.com/a.png\"/>"));
        assert!(refused("<image href=\"icon.png\"/>"));
        assert!(refused(
            "<filter id=\"f\"><feImage href=\"/etc/passwd\"/></filter>"
        ));
    }

    #[test]
    fn external_use_is_refused() {
        assert!(refused("<use href=\"other.svg#x\"/>"));
        assert!(refused("<use xlink:href=\"/tmp/a.svg#x\"/>"));
        assert!(!refused("<defs><rect id=\"x\"/></defs><use href=\"#x\"/>"));
        assert!(!refused(
            "<defs><rect id=\"x\"/></defs><use xlink:href=\" #x \"/>"
        ));
    }

    #[test]
    fn css_urls_are_refused() {
        assert!(refused("<rect fill=\"url(/x)\" width=\"1\" height=\"1\"/>"));
        assert!(refused("<rect style=\"fill: url( 'file:///x' )\"/>"));
        assert!(refused("<style>rect { fill: url(\"/x.svg#g\") }</style>"));
        assert!(refused(
            "<style><![CDATA[ rect { mask: URL(a.svg) } ]]></style>"
        ));
        assert!(refused("<style>@import \"a.css\";</style>"));
        assert!(refused("<style>@im/**/port url(a.css);</style>"));
        assert!(refused("<style>rect { fill: u\\72l(/x) }</style>"));
        assert!(!refused(
            "<defs><linearGradient id=\"g\"/></defs>\
             <style>rect { fill: url(#g) }</style><rect fill=\"url( '#g' )\"/>"
        ));
        assert!(!refused(
            "<style>rect { fill: url(data:image/png;base64,AAAA) }</style>"
        ));
        // Escapes in an ordinary font name are fine.
        assert!(!refused(
            "<style>text { font-family: \"\\5FAE\\8F6F\\96C5\\9ED1\" }</style>"
        ));
    }

    #[test]
    fn stylesheet_instruction_is_refused() {
        let bytes = b"<?xml version=\"1.0\"?><?xml-stylesheet href=\"a.css\"?>\
                      <svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert!(matches!(
            check(bytes),
            Err(ImageError::ExternalReference(_))
        ));
    }

    #[test]
    fn references_behind_entities_are_seen() {
        let bytes = b"<?xml version=\"1.0\"?>\
            <!DOCTYPE svg [<!ENTITY p \"/etc/passwd\">]>\
            <svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"&p;\"/></svg>";
        assert!(matches!(
            check(bytes),
            Err(ImageError::ExternalReference(_))
        ));
        // External entities are never loaded: the document doesn't parse.
        let bytes = b"<?xml version=\"1.0\"?>\
            <!DOCTYPE svg [<!ENTITY p SYSTEM \"file:///etc/passwd\">]>\
            <svg xmlns=\"http://www.w3.org/2000/svg\"><text>&p;</text></svg>";
        assert!(check(bytes).is_err());
    }

    #[test]
    fn data_urls_and_links_stay() {
        assert!(!refused(
            "<image width=\"1\" height=\"1\" href=\"data:image/png;base64,AAAA\"/>"
        ));
        assert!(!refused(
            "<image xlink:href=\"DATA:image/svg+xml,%3Csvg/%3E\"/>"
        ));
        assert!(!refused(
            "<a href=\"https://example.com\"><rect width=\"1\" height=\"1\"/></a>"
        ));
        assert!(!refused("<a xlink:href=\"/local/page\"><text>x</text></a>"));
        assert!(!refused("<rect width=\"1\" height=\"1\" fill=\"#fff\"/>"));
    }
}
