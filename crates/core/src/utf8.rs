/// Strict, complete-only text decoding without copying the owned byte buffer.
/// Each unit validates at most 8 KiB, retaining incomplete scalar bytes for the
/// next unit. Invalid text is never replaced or exposed as a partial String.
pub struct Utf8TextPreparation {
    bytes: Vec<u8>,
    offset: usize,
    result: Option<Result<(), InvalidUtf8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidUtf8 {
    pub offset: usize,
    pub length: Option<usize>,
}
impl std::fmt::Display for InvalidUtf8 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Invalid UTF-8 at byte {}", self.offset)
    }
}
impl std::error::Error for InvalidUtf8 {}

impl Utf8TextPreparation {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            result: bytes.is_empty().then_some(Ok(())),
            bytes,
            offset: 0,
        }
    }
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            let end = (self.offset + 8192).min(self.bytes.len());
            match std::str::from_utf8(&self.bytes[self.offset..end]) {
                Ok(_) => self.offset = end,
                Err(error) if error.error_len().is_none() && end < self.bytes.len() => {
                    self.offset += error.valid_up_to();
                }
                Err(error) => {
                    self.result = Some(Err(InvalidUtf8 {
                        offset: self.offset + error.valid_up_to(),
                        length: error.error_len(),
                    }));
                }
            }
            if self.offset == self.bytes.len() {
                self.result = Some(Ok(()));
            }
        }
        self.result.is_some()
    }
    pub fn finish(self) -> Option<Result<String, InvalidUtf8>> {
        Some(self.result?.map(|()| {
            // SAFETY: bytes are privately owned and never mutated. Every byte
            // was validated by from_utf8 in contiguous complete-scalar slices;
            // successful completion requires reaching the entire buffer's end.
            unsafe { String::from_utf8_unchecked(self.bytes) }
        }))
    }
}

pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        let mut out = String::new();
        self.pending.extend_from_slice(bytes);

        let mut offset = 0;
        while offset < self.pending.len() {
            match std::str::from_utf8(&self.pending[offset..]) {
                Ok(s) => {
                    out.push_str(s);
                    offset = self.pending.len();
                    break;
                }
                Err(e) => {
                    let valid_len = e.valid_up_to();
                    if valid_len > 0 {
                        // We have some valid text before the error
                        let valid_str = unsafe {
                            std::str::from_utf8_unchecked(&self.pending[offset..offset + valid_len])
                        };
                        out.push_str(valid_str);
                        offset += valid_len;
                        continue;
                    }

                    // No valid prefix. Is it an incomplete sequence at the end, or an actual error?
                    match e.error_len() {
                        Some(err_len) => {
                            // Real error of length err_len
                            out.push('\u{FFFD}');
                            offset += err_len;
                        }
                        None => {
                            // Incomplete sequence at the end of the slice (up to 3 bytes long).
                            break;
                        }
                    }
                }
            }
        }

        self.pending.drain(..offset);
        out
    }

    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        if !self.pending.is_empty() {
            // Whatever is left is incomplete and thus invalid. We just emit FFFD for each invalid chunk if we want to strictly follow lossy rules,
            // but `String::from_utf8_lossy` does exactly what we need for a trailing sequence.
            out.push_str(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
        out
    }
}

impl Default for Utf8Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_preparation_preserves_utf8_errors_ownership_and_unit_bounds() {
        for padding in [0, 8189, 8190, 8191, 8192] {
            for tail in [
                "文😀e\u{301}\r\n".as_bytes(),
                &[0xff],
                &[0xf0, 0x9f],
                &[0xe0, 0x80, 0x80],
            ] {
                let mut bytes = vec![b'a'; padding];
                bytes.extend_from_slice(tail);
                bytes.extend_from_slice(b" tail");
                let reference = String::from_utf8(bytes.clone());
                let pointer = bytes.as_ptr();
                let mut preparation = Utf8TextPreparation::new(bytes);
                assert!(!preparation.advance(0));
                while preparation.result.is_none() {
                    let before = preparation.offset;
                    preparation.advance(1);
                    assert!(preparation.offset - before <= 8192);
                }
                match (preparation.finish().unwrap(), reference) {
                    (Ok(actual), Ok(expected)) => {
                        assert_eq!(actual, expected);
                        assert_eq!(actual.as_ptr(), pointer);
                    }
                    (Err(actual), Err(expected)) => {
                        assert_eq!(actual.offset, expected.utf8_error().valid_up_to());
                        assert_eq!(actual.length, expected.utf8_error().error_len());
                    }
                    _ => panic!("strict decoding disagreed with the standard library"),
                }
            }
        }
        let mut preparation = Utf8TextPreparation::new(vec![b'x'; 32 * 1024]);
        assert!(!preparation.advance(1));
        assert!(preparation.finish().is_none());
        assert_eq!(
            Utf8TextPreparation::new(Vec::new()).finish(),
            Some(Ok(String::new()))
        );
        let mut bytes = vec![b'x'; 8191];
        bytes.extend_from_slice(&[0xf0, 0x9f]);
        let mut preparation = Utf8TextPreparation::new(bytes);
        while !preparation.advance(1) {}
        assert_eq!(
            preparation.finish(),
            Some(Err(InvalidUtf8 {
                offset: 8191,
                length: None
            }))
        );
    }

    #[test]
    fn test_utf8_decoder() {
        // Euro sign is 3 bytes: E2 82 AC
        // Emoji 🎉 is 4 bytes: F0 9F 8E 89
        let text = "café 日本 🎉";
        for i in 0..text.len() {
            let mut d2 = Utf8Decoder::new();
            let mut res = String::new();
            res.push_str(&d2.push(&text.as_bytes()[..i]));
            res.push_str(&d2.push(&text.as_bytes()[i..]));
            res.push_str(&d2.finish());
            assert_eq!(res, text);
        }

        // 0xFF then text -> one U+FFFD then the text
        let mut d = Utf8Decoder::new();
        let mut res = String::new();
        res.push_str(&d.push(&[0xFF]));
        res.push_str(&d.push(b"hello"));
        res.push_str(&d.finish());
        assert_eq!(res, "\u{FFFD}hello");

        // finish on a dangling lead byte
        let mut d = Utf8Decoder::new();
        let mut res = String::new();
        res.push_str(&d.push(&[0xE2, 0x82]));
        res.push_str(&d.finish());
        assert_eq!(res, "\u{FFFD}"); // from_utf8_lossy on [0xE2, 0x82] replaces with one FFFD
    }
}
