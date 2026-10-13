//! Compatibility helpers for JSON archives written by Node.js.
//!
//! JavaScript strings can contain lone UTF-16 surrogate code units, and
//! `JSON.parse` accepts their `\uXXXX` escapes. Rust `String` cannot represent
//! those values, so replace only unpaired escaped surrogates with U+FFFD before
//! parsing. Valid surrogate pairs and all non-surrogate bytes remain unchanged.

pub(crate) fn normalize_node_json(input: &[u8]) -> Vec<u8> {
    fn hex_quad(bytes: &[u8]) -> Option<u16> {
        if bytes.len() < 4 {
            return None;
        }
        bytes[..4].iter().try_fold(0_u16, |value, byte| {
            let digit = match byte {
                b'0'..=b'9' => u16::from(*byte - b'0'),
                b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                _ => return None,
            };
            Some(value * 16 + digit)
        })
    }

    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    while index < input.len() {
        let byte = input[index];
        if !in_string {
            output.push(byte);
            if byte == b'"' {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            output.push(byte);
            in_string = false;
            index += 1;
            continue;
        }
        if byte != b'\\' || input.get(index + 1) != Some(&b'u') {
            output.push(byte);
            if byte == b'\\' && index + 1 < input.len() {
                index += 1;
                output.push(input[index]);
            }
            index += 1;
            continue;
        }

        let Some(code_unit) = input.get(index + 2..index + 6).and_then(hex_quad) else {
            output.push(byte);
            index += 1;
            continue;
        };
        if (0xD800..=0xDBFF).contains(&code_unit) {
            let next = input.get(index + 6..index + 12);
            let paired_low = next
                .filter(|bytes| bytes.get(..2) == Some(b"\\u"))
                .and_then(|bytes| hex_quad(&bytes[2..]))
                .is_some_and(|low| (0xDC00..=0xDFFF).contains(&low));
            if paired_low {
                output.extend_from_slice(&input[index..index + 12]);
                index += 12;
            } else {
                output.extend_from_slice("�".as_bytes());
                index += 6;
            }
        } else if (0xDC00..=0xDFFF).contains(&code_unit) {
            output.extend_from_slice("�".as_bytes());
            index += 6;
        } else {
            output.extend_from_slice(&input[index..index + 6]);
            index += 6;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::normalize_node_json;

    #[test]
    fn replaces_only_unpaired_surrogate_escapes_inside_json_strings() {
        let input = br#"{"high":"\uD800","low":"\uDC00","pair":"\uD83D\uDE00","literal":"\\uD800","number":123}"#;
        let normalized = normalize_node_json(input);
        let value: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
        assert_eq!(value["high"], "�");
        assert_eq!(value["low"], "�");
        assert_eq!(value["pair"], "😀");
        assert_eq!(value["literal"], r"\uD800");
        assert_eq!(value["number"], 123);
    }
}
