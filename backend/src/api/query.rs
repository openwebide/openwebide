use spin_sdk::http::Request;
use std::collections::HashMap;
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(super) fn query<B>(req: &Request<B>) -> HashMap<String, String> {
    parse_query(req.uri().query().unwrap_or_default())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            result
                .entry(urldecode(key))
                .or_insert_with(|| urldecode(value));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_literal_plus_in_file_paths() {
        for (encoded, expected) in [
            ("a+b.txt", "a+b.txt"),
            ("a%2Bb.txt", "a+b.txt"),
            ("a%20b.txt", "a b.txt"),
            ("a%252Bb.txt", "a%2Bb.txt"),
        ] {
            let params = parse_query(&format!("path={encoded}"));
            assert_eq!(params.get("path").unwrap(), expected);
        }
    }

    #[test]
    fn decodes_once() {
        assert_eq!(urldecode("a%20b%2Fc%252F"), "a b/c%2F");
        assert_eq!(urldecode("%E2%98%83"), "☃");
        assert_eq!(urldecode("broken%2"), "broken%2");
        let params = parse_query("path=a%252Fb&mo%64el=a+b&path=other");
        assert_eq!(params.get("path").unwrap(), "a%2Fb");
        assert_eq!(params.get("model").unwrap(), "a+b");
    }
}
