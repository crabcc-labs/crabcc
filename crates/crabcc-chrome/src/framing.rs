//! Chrome native-messaging frame codec.
//!
//! Wire format: a single message is a 4-byte little-endian length prefix
//! followed by exactly that many bytes of UTF-8 JSON. Both directions on
//! stdin/stdout. Max message size per spec is 1 MB from extension to host
//! and 64 MB from host to extension; we cap at [`MAX_FRAME_SIZE`] to
//! refuse pathological inputs without OOM.
//!
//! See: https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging

use std::io::{self, Read, Write};

/// Refuse frames larger than 64 MiB — matches Chrome's host-→-extension
/// upper bound. Anything bigger is almost certainly malformed.
pub const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// Read one native-messaging frame from `r`. Returns `Ok(None)` on a
/// clean EOF (zero bytes available before the length prefix) so callers
/// can drain a closed pipe without treating it as a parse error.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds {MAX_FRAME_SIZE}"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Write `body` as a single native-messaging frame to `w`. Caller is
/// responsible for `w.flush()` at appropriate points — we don't flush
/// here so back-to-back writes can coalesce.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {} exceeds {MAX_FRAME_SIZE}", body.len()),
        ));
    }
    let len = (body.len() as u32).to_le_bytes();
    w.write_all(&len)?;
    w.write_all(body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_small() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"hello\":1}").unwrap();
        // 4 bytes length + 11 bytes body = 15.
        assert_eq!(buf.len(), 15);
        assert_eq!(&buf[0..4], &11u32.to_le_bytes());

        let mut cur = Cursor::new(&buf);
        let frame = read_frame(&mut cur).unwrap().unwrap();
        assert_eq!(&frame, b"{\"hello\":1}");
    }

    #[test]
    fn read_returns_none_on_clean_eof() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert!(matches!(read_frame(&mut cur), Ok(None)));
    }

    #[test]
    fn read_rejects_oversized_length() {
        let mut buf = Vec::new();
        // Write a length that exceeds MAX_FRAME_SIZE.
        buf.extend_from_slice(&((MAX_FRAME_SIZE as u32) + 1).to_le_bytes());
        let mut cur = Cursor::new(&buf);
        let err = read_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_returns_none_on_partial_length() {
        // Truncated stream is treated as EOF — same path as a clean shutdown.
        // Distinguishing them isn't useful for our consumers (both mean
        // "the other side went away"), and unifies error handling.
        let mut cur = Cursor::new(vec![0x05, 0x00]); // only 2 of 4 bytes
                                                     // read_exact returns UnexpectedEof which we promote to Ok(None).
        match read_frame(&mut cur) {
            Ok(None) => {}
            other => panic!("expected Ok(None), got {other:?}"),
        }
    }

    #[test]
    fn write_rejects_oversized_body() {
        let body = vec![0u8; MAX_FRAME_SIZE + 1];
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, &body).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
