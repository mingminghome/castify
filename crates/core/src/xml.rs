//! Small XML helpers for UPnP SOAP / DIDL. Not a full parser.

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if text[i..].starts_with("&amp;") {
                out.push('&');
                i += 5;
                continue;
            }
            if text[i..].starts_with("&lt;") {
                out.push('<');
                i += 4;
                continue;
            }
            if text[i..].starts_with("&gt;") {
                out.push('>');
                i += 4;
                continue;
            }
            if text[i..].starts_with("&quot;") {
                out.push('"');
                i += 6;
                continue;
            }
            if text[i..].starts_with("&apos;") {
                out.push('\'');
                i += 6;
                continue;
            }
            if text[i..].starts_with("&#10;") {
                out.push('\n');
                i += 5;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// First inner text of `<name>...</name>` (case-insensitive tag name).
pub fn tag_text(xml: &str, name: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let open = format!("<{}", name.to_ascii_lowercase());
    let close = format!("</{}>", name.to_ascii_lowercase());
    let mut start = 0;
    while let Some(rel) = lower[start..].find(&open) {
        let abs = start + rel;
        let after_name = abs + open.len();
        let rest = &xml[after_name..];
        let rest_lower = &lower[after_name..];
        if rest_lower.starts_with('>') || rest_lower.starts_with(' ') || rest_lower.starts_with('\t')
        {
            if let Some(gt) = rest.find('>') {
                let content_at = after_name + gt + 1;
                if let Some(end_rel) = lower[content_at..].find(&close) {
                    let inner = xml[content_at..content_at + end_rel].trim();
                    return Some(unescape(inner));
                }
            }
        }
        start = after_name;
    }
    None
}

pub fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{}=", name.to_ascii_lowercase());
    let idx = lower.find(&key)?;
    let after = &tag[idx + key.len()..];
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[1..];
    let end = rest.find(quote)?;
    Some(unescape(&rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_entities() {
        let raw = r#"a&b<c>"d"'e"#;
        assert_eq!(unescape(&escape(raw)), raw);
    }

    #[test]
    fn reads_soap_arg() {
        let xml = r#"<u:SetAVTransportURI xmlns:u="urn:x"><InstanceID>0</InstanceID><CurrentURI>http://10.0.0.2/p.jpg</CurrentURI></u:SetAVTransportURI>"#;
        assert_eq!(
            tag_text(xml, "CurrentURI").as_deref(),
            Some("http://10.0.0.2/p.jpg")
        );
    }
}
