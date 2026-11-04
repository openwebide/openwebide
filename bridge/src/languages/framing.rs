//! LSP stdio framing. Bound headers before allocation and count body bytes,
//! independently of UTF-8 character count or process pipe chunk boundaries.
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_HEADER_BYTES: usize = 8192;
pub const MAX_INCOMING_BYTES: usize =
    openwebide_core::plugins::languages::intelligence::MAX_RESULT_BYTES;
pub const MAX_OUTGOING_BYTES: usize = openwebide_core::plugins::languages::MAX_REQUEST_BYTES;

pub async fn read(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Value, String> {
    let mut header_bytes = 0;
    let mut length = None;
    let mut content_type = false;
    loop {
        let mut line = Vec::new();
        loop {
            let bytes = reader.fill_buf().await.map_err(|error| error.to_string())?;
            if bytes.is_empty() {
                return Err("Language server closed its output pipe".into());
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            header_bytes += count;
            if header_bytes > MAX_HEADER_BYTES {
                return Err("Language-server header exceeds its byte limit".into());
            }
            line.extend_from_slice(&bytes[..count]);
            let complete = bytes[count - 1] == b'\n';
            reader.consume(count);
            if complete {
                break;
            }
        }
        let line = line
            .strip_suffix(b"\r\n")
            .ok_or("Language-server headers require CRLF")?;
        if line.is_empty() {
            break;
        }
        if !line.is_ascii() || line.iter().any(|b| b.is_ascii_control() && *b != b'\t') {
            return Err("Invalid language-server header encoding".into());
        }
        let line = std::str::from_utf8(line).map_err(|error| error.to_string())?;
        let (name, value) = line
            .split_once(':')
            .ok_or("Invalid language-server header")?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Invalid or duplicate language-server Content-Length".into());
            }
            let size = value
                .parse::<usize>()
                .map_err(|_| "Invalid language-server body length")?;
            if size == 0 || size > MAX_INCOMING_BYTES {
                return Err("Language-server message exceeds its byte limit".into());
            }
            length = Some(size);
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type {
                return Err("Duplicate language-server Content-Type".into());
            }
            content_type = true;
            for parameter in value.split(';').skip(1) {
                if let Some((key, encoding)) = parameter.trim().split_once('=')
                    && key.eq_ignore_ascii_case("charset")
                    && !matches!(
                        encoding.trim().to_ascii_lowercase().as_str(),
                        "utf-8" | "utf8"
                    )
                {
                    return Err("Language-server messages require UTF-8".into());
                }
            }
        }
    }
    let mut bytes = vec![0; length.ok_or("Language-server message has no Content-Length")?];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|error| format!("Incomplete language-server message: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid language-server JSON: {error}"))
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), message: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_OUTGOING_BYTES {
        return Err("Language-server request exceeds its byte limit".into());
    }
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn unicode_and_multiple_messages_survive_fragmented_pipe_reads() {
        let first = serde_json::json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"message":"水😀"}});
        let second = serde_json::json!({"jsonrpc":"2.0","id":7,"result":null});
        let mut bytes = Vec::new();
        write(&mut bytes, &first).await.unwrap();
        write(&mut bytes, &second).await.unwrap();
        let (mut output, input) = tokio::io::duplex(7);
        let writer = tokio::spawn(async move {
            for chunk in bytes.chunks(3) {
                output.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut input = BufReader::with_capacity(5, input);
        assert_eq!(read(&mut input).await.unwrap(), first);
        assert_eq!(read(&mut input).await.unwrap(), second);
        writer.await.unwrap();
        assert!(read(&mut input).await.is_err());
    }

    #[tokio::test]
    async fn invalid_lengths_headers_encoding_and_truncation_are_rejected() {
        let oversized = format!("Content-Length: {}\r\n\r\n", MAX_INCOMING_BYTES + 1);
        let long_header = format!(
            "X-Header: {}\r\nContent-Length: 2\r\n\r\n{{}}",
            "x".repeat(MAX_HEADER_BYTES)
        );
        for bytes in [
            b"\r\n{}".as_slice(),
            b"Content-Length: 0\r\n\r\n",
            b"Content-Length: -1\r\n\r\n",
            b"Content-Length: 9999999999999999999999999999\r\n\r\n",
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            b"Content-Length: 2\n\n{}",
            b"Content-Length: 4\r\n\r\n{}",
            b"Content-Length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-16\r\n\r\n{}",
            b"Content-Length: 2\r\nContent-Type: x\r\nContent-Type: x\r\n\r\n{}",
            b"Content-Length: 2\r\n\r\n\xff\xff",
            oversized.as_bytes(),
            long_header.as_bytes(),
        ] {
            assert!(read(&mut &bytes[..]).await.is_err(), "accepted {}", String::from_utf8_lossy(bytes));
        }
        let mut valid = &b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf8\r\n\r\n{}"[..];
        assert_eq!(read(&mut valid).await.unwrap(), serde_json::json!({}));
    }
}
