//! A guard in front of `tower-lsp-server`'s stdio transport (W4-11).
//!
//! `tower-lsp-server` 0.23 decodes frames with a `tokio_util` `FramedRead`,
//! which ends the stream after the first decode error. One malformed frame —
//! truncated JSON, non-JSON, a JSON array, a message without `"jsonrpc"`,
//! non-UTF-8 bytes — therefore got an error reply and then stopped the server,
//! with exit code 0, which can stop a client from restarting it.
//!
//! The guard reads frames itself and only forwards bodies the library decodes
//! (a JSON-RPC 2.0 request, notification or response). Anything else is
//! answered here with the JSON-RPC error the spec prescribes — `-32700` for
//! bytes that are not a JSON value, `-32600` for a JSON value that is not a
//! valid message — and the server keeps reading. Replies from the server and
//! from the guard reach stdout through one writer, a whole frame at a time,
//! so they never interleave.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tower_lsp_server::jsonrpc::{Request, Response};

/// Bodies larger than this are rejected (and skipped) rather than buffered.
const MAX_BODY: usize = 256 * 1024 * 1024;

/// One frame read from the client.
#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    /// A complete body, as announced by `Content-Length`.
    Body(Vec<u8>),
    /// A header block that announced no usable body; the reason.
    BadHeader(String),
}

/// Read one LSP frame. `Ok(None)` at end of input, including input that ends
/// inside a frame (there is nothing left to answer).
pub(crate) async fn read_frame<R: AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<Frame>> {
    let mut content_length: Option<Result<usize, String>> = None;
    let mut saw_header = false;
    loop {
        let mut line = Vec::new();
        if r.read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            if saw_header {
                break;
            }
            // Stray blank line between frames.
            continue;
        }
        saw_header = true;
        if let Some((name, value)) = text.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| format!("invalid Content-Length {:?}: {e}", value.trim())),
                );
            }
        }
    }
    let len = match content_length {
        None => {
            return Ok(Some(Frame::BadHeader(
                "missing Content-Length header".into(),
            )))
        }
        Some(Err(why)) => return Ok(Some(Frame::BadHeader(why))),
        Some(Ok(len)) => len,
    };
    if len > MAX_BODY {
        // Skip the body without buffering it.
        let skipped = tokio::io::copy(&mut r.take(len as u64), &mut tokio::io::sink()).await?;
        if (skipped as usize) < len {
            return Ok(None);
        }
        return Ok(Some(Frame::BadHeader(format!(
            "Content-Length {len} exceeds the {MAX_BODY}-byte limit"
        ))));
    }
    let mut body = vec![0u8; len];
    match r.read_exact(&mut body).await {
        Ok(_) => Ok(Some(Frame::Body(body))),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Encode `body` as a frame.
pub(crate) fn encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// `Ok(())` when `tower-lsp-server` will decode `body`; otherwise the error
/// reply the client should get instead.
pub(crate) fn vet(body: &[u8]) -> Result<(), serde_json::Value> {
    let error = |code: i64, message: String, id: serde_json::Value| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        })
    };
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            return Err(error(
                -32700,
                format!("Parse error: body is not UTF-8 ({e})"),
                serde_json::Value::Null,
            ))
        }
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return Err(error(
                -32700,
                format!("Parse error: {e}"),
                serde_json::Value::Null,
            ))
        }
    };
    // The id to answer with, when the message carries a usable one.
    let id = value
        .get("id")
        .filter(|id| id.is_number() || id.is_string())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if !value.is_object() {
        return Err(error(
            -32600,
            "Invalid Request: expected a JSON-RPC message object (batches are not supported)"
                .into(),
            id,
        ));
    }
    if serde_json::from_value::<Request>(value.clone()).is_ok()
        || serde_json::from_value::<Response>(value.clone()).is_ok()
    {
        return Ok(());
    }
    let why = if value.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        "missing or unsupported \"jsonrpc\" version (expected \"2.0\")"
    } else {
        "not a JSON-RPC request, notification or response"
    };
    Err(error(-32600, format!("Invalid Request: {why}"), id))
}

/// Copy client frames from `input` to `server`, answering malformed ones on
/// `replies`. Returns at end of input (dropping `server` closes the server's
/// input, which ends `serve`).
pub(crate) async fn guard_input<R, W>(
    mut input: R,
    mut server: W,
    replies: mpsc::UnboundedSender<Vec<u8>>,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let frame = match read_frame(&mut input).await {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => return,
        };
        let reply = match frame {
            Frame::Body(body) => match vet(&body) {
                Ok(()) => {
                    if server.write_all(&encode(&body)).await.is_err()
                        || server.flush().await.is_err()
                    {
                        return;
                    }
                    continue;
                }
                Err(reply) => reply,
            },
            Frame::BadHeader(why) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32700, "message": format!("Parse error: {why}")},
            }),
        };
        let body = serde_json::to_vec(&reply).unwrap_or_default();
        if replies.send(encode(&body)).is_err() {
            return;
        }
    }
}

/// Forward the server's output frames to `replies`, whole.
pub(crate) async fn relay_output<R>(mut output: R, replies: mpsc::UnboundedSender<Vec<u8>>)
where
    R: AsyncBufRead + Unpin,
{
    while let Ok(Some(Frame::Body(body))) = read_frame(&mut output).await {
        if replies.send(encode(&body)).is_err() {
            return;
        }
    }
}

/// Write every frame from `frames` to `out` until all senders are gone.
pub(crate) async fn write_frames<W: AsyncWrite + Unpin>(
    mut out: W,
    mut frames: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(frame) = frames.recv().await {
        if out.write_all(&frame).await.is_err() || out.flush().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vet_classifies_bodies() {
        assert!(vet(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).is_ok());
        assert!(vet(br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#).is_ok());
        assert!(vet(br#"{"jsonrpc":"2.0","id":3,"result":null}"#).is_ok());
        let code = |b: &[u8]| vet(b).unwrap_err()["error"]["code"].as_i64().unwrap();
        assert_eq!(code(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"me"), -32700);
        assert_eq!(code(b"hello"), -32700);
        assert_eq!(code(b"\xff\xfe"), -32700);
        assert_eq!(code(b"[]"), -32600);
        assert_eq!(code(br#"{"id":5,"method":"x"}"#), -32600);
        assert_eq!(vet(br#"{"id":5,"method":"x"}"#).unwrap_err()["id"], 5);
    }

    #[tokio::test]
    async fn read_frame_handles_headers_and_eof() {
        let input: &[u8] =
            b"Content-Length: 2\r\n\r\n{}\r\nX-Other: 1\r\n\r\nContent-Length: 5\r\n\r\nab";
        let mut r = tokio::io::BufReader::new(input);
        assert_eq!(
            read_frame(&mut r).await.unwrap(),
            Some(Frame::Body(b"{}".to_vec()))
        );
        assert!(matches!(
            read_frame(&mut r).await.unwrap(),
            Some(Frame::BadHeader(_))
        ));
        // Truncated body: end of input.
        assert_eq!(read_frame(&mut r).await.unwrap(), None);
    }
}
