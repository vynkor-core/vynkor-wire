use crate::error::WireError;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_PAYLOAD_SIZE: usize = 1_048_576;
const MAGIC: u16 = 0x5652;
const HEADER_SIZE: usize = 44;

/// `flags` bit indicating a 32-byte HMAC tag is appended after the payload.
pub const FLAG_MAC_PRESENT: u16 = 0x0001;

/// Payload is zstd-compressed. Decompressed by framing layer before delivery.
/// CRC32 is computed over the compressed bytes (what is on the wire).
pub const FLAG_COMPRESSED: u16 = 0x0002;

/// Payload is raw binary (PCM/Opus audio). Router skips Protobuf decode.
pub const FLAG_RAW_BINARY: u16 = 0x0010;

/// Frame is one fragment of a larger message. The first [`FRAG_HEADER_SIZE`]
/// bytes of the payload contain fragment metadata; the remainder is the chunk.
pub const FLAG_FRAGMENTED: u16 = 0x0004;

/// Payloads at or above this size are candidates for zstd compression.
pub const COMPRESS_THRESHOLD: usize = 65_536;

/// Byte length of the fragment metadata header embedded at the start of a
/// fragmented frame's payload when [`FLAG_FRAGMENTED`] is set.
///
/// Layout (all big-endian):
///   [fragment_id: u16][sequence: u16][total: u16][stream_id: u32]
pub const FRAG_HEADER_SIZE: usize = 10;

/// Parsed representation of the 10-byte fragment metadata header.
#[derive(Debug, Clone, Copy)]
pub struct FragmentHeader {
    /// Opaque identifier for the fragmented message within a stream.
    /// Parsed from the wire; available for callers but not used by the kernel.
    pub fragment_id: u16,
    /// Zero-based position of this fragment in the sequence.
    pub sequence: u16,
    /// Total number of fragments that make up the original message.
    pub total: u16,
    /// Stream identifier used as the reassembly buffer key.
    pub stream_id: u32,
}

/// Parses the [`FragmentHeader`] from the start of a frame payload.
/// Returns `None` if the payload is shorter than [`FRAG_HEADER_SIZE`].
pub fn parse_frag_header(payload: &[u8]) -> Option<FragmentHeader> {
    if payload.len() < FRAG_HEADER_SIZE {
        return None;
    }
    Some(FragmentHeader {
        fragment_id: u16::from_be_bytes([payload[0], payload[1]]),
        sequence: u16::from_be_bytes([payload[2], payload[3]]),
        total: u16::from_be_bytes([payload[4], payload[5]]),
        stream_id: u32::from_be_bytes([payload[6], payload[7], payload[8], payload[9]]),
    })
}

/// Once a frame has started arriving, the rest of the header + payload must
/// complete within this window. Bounds slow-loris stalls (a peer that sends a
/// header declaring a large payload then dribbles or stops). Idle connections
/// waiting for the next frame are NOT subject to it.
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Frame {
    pub magic: u16,
    pub flags: u16,
    pub length: u32,
    pub target: [u8; 32],
    pub crc32: u32,
    /// Shared, immutable payload bytes. `Arc<[u8]>` so fan-out to N subscribers
    /// (broadcast, event bus) and per-write framing clone the reference, not
    /// the bytes.
    pub payload: Arc<[u8]>,
    /// 32-byte HMAC tag, present iff `flags & FLAG_MAC_PRESENT != 0`.
    pub mac: Option<[u8; 32]>,
}

/// Serialize the 44-byte frame header exactly as it goes on the wire. Used by
/// both `write_frame_raw` and MAC computation so the tag covers the real bytes.
pub fn serialize_header(frame: &Frame) -> [u8; HEADER_SIZE] {
    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&frame.magic.to_be_bytes());
    header[2..4].copy_from_slice(&frame.flags.to_be_bytes());
    header[4..8].copy_from_slice(&frame.length.to_be_bytes());
    header[8..40].copy_from_slice(&frame.target);
    header[40..44].copy_from_slice(&frame.crc32.to_be_bytes());
    header
}

pub async fn write_frame<W>(
    stream: &mut W,
    target: &str,
    flags: u16,
    payload: &[u8],
) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(WireError::PayloadTooLarge(payload.len()));
    }

    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&MAGIC.to_be_bytes());
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());

    let target_bytes = target.as_bytes();
    let copy_len = target_bytes.len().min(32);
    header[8..8 + copy_len].copy_from_slice(&target_bytes[..copy_len]);

    let checksum = crc32fast::hash(payload);
    header[40..44].copy_from_slice(&checksum.to_be_bytes());

    stream.write_all(&header).await?;
    stream.write_all(payload).await?;
    Ok(())
}

pub async fn write_frame_raw<W>(stream: &mut W, frame: &Frame) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
{
    if frame.payload.len() > MAX_PAYLOAD_SIZE {
        return Err(WireError::PayloadTooLarge(frame.payload.len()));
    }

    // PERF-4: compute CRC only when compression changes the payload.
    // When no compression occurs (below threshold, already compressed, or raw
    // binary), the existing frame.crc32 is valid — skip the hash entirely.
    let (wire_payload, wire_flags, wire_crc): (Arc<[u8]>, u16, u32) = if frame.payload.len()
        >= COMPRESS_THRESHOLD
        && frame.flags & FLAG_COMPRESSED == 0
        && frame.flags & FLAG_RAW_BINARY == 0
    {
        match zstd::bulk::compress(&frame.payload, 3) {
            Ok(c) if c.len() < frame.payload.len() => {
                let compressed = Arc::from(c);
                let crc = crc32fast::hash(&compressed);
                (compressed, frame.flags | FLAG_COMPRESSED, crc)
            }
            _ => (frame.payload.clone(), frame.flags, frame.crc32),
        }
    } else {
        (frame.payload.clone(), frame.flags, frame.crc32)
    };

    let wire_frame = Frame {
        magic: frame.magic,
        flags: wire_flags,
        length: wire_payload.len() as u32,
        target: frame.target,
        crc32: wire_crc,
        payload: wire_payload,
        mac: frame.mac,
    };
    let header = serialize_header(&wire_frame);

    stream.write_all(&header).await?;
    stream.write_all(&wire_frame.payload).await?;
    if let Some(tag) = &wire_frame.mac {
        stream.write_all(tag).await?;
    }
    Ok(())
}

pub async fn read_frame<R>(stream: &mut R) -> Result<Frame, WireError>
where
    R: AsyncRead + Unpin,
{
    read_frame_with_timeout(stream, FRAME_READ_TIMEOUT).await
}

pub async fn read_frame_with_timeout<R>(
    stream: &mut R,
    frame_timeout: Duration,
) -> Result<Frame, WireError>
where
    R: AsyncRead + Unpin,
{
    // Block indefinitely for the first byte — an idle connection between frames
    // must not be torn down. Once a byte arrives, a frame is in progress and the
    // remainder is bounded by frame_timeout.
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await?;

    match tokio::time::timeout(frame_timeout, read_frame_body(stream, first[0])).await {
        Ok(result) => result,
        Err(_) => Err(WireError::FrameReadTimeout),
    }
}

async fn read_frame_body<R>(stream: &mut R, first_byte: u8) -> Result<Frame, WireError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; HEADER_SIZE];
    header[0] = first_byte;
    stream.read_exact(&mut header[1..]).await?;

    let magic = u16::from_be_bytes([header[0], header[1]]);
    if magic != MAGIC {
        return Err(WireError::FrameMagicMismatch);
    }

    let flags = u16::from_be_bytes([header[2], header[3]]);
    let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);

    if length as usize > MAX_PAYLOAD_SIZE {
        return Err(WireError::PayloadTooLarge(length as usize));
    }

    let mut target = [0u8; 32];
    target.copy_from_slice(&header[8..40]);

    let crc32 = u32::from_be_bytes([header[40], header[41], header[42], header[43]]);

    let mut payload = vec![0u8; length as usize];
    if length > 0 {
        stream.read_exact(&mut payload).await?;
    }

    // CRC is over the wire bytes (possibly compressed); verify before decompressing.
    let computed = crc32fast::hash(&payload);
    if computed != crc32 {
        return Err(WireError::FrameCrcMismatch);
    }

    // Normalize the in-memory invariant: payload is always plaintext, and
    // flags/length/crc32 describe the plaintext, regardless of what was on the
    // wire. The MAC (if any) was computed by the sender over the pre-compression
    // header+payload, so crc32 must be recomputed over the decompressed bytes —
    // the wire crc32 (over compressed bytes) would fail verification.
    let (payload, flags, length, crc32) = if flags & FLAG_COMPRESSED != 0 {
        let decompressed = zstd::bulk::decompress(&payload, MAX_PAYLOAD_SIZE)
            .map_err(|e| WireError::Internal(format!("decompress frame: {e}")))?;
        let plain_len = decompressed.len() as u32;
        let plain_crc = crc32fast::hash(&decompressed);
        (decompressed, flags & !FLAG_COMPRESSED, plain_len, plain_crc)
    } else {
        (payload, flags, length, crc32)
    };

    let mac = if flags & FLAG_MAC_PRESENT != 0 {
        let mut tag = [0u8; 32];
        stream.read_exact(&mut tag).await?;
        Some(tag)
    } else {
        None
    };

    Ok(Frame {
        magic,
        flags,
        length,
        target,
        crc32,
        payload: payload.into(),
        mac,
    })
}

/// Returns `None` if the target bytes are not valid UTF-8. Callers must log
/// the raw hex and return an error frame in that case (VULN-022).
pub fn target_as_str(frame: &Frame) -> Option<&str> {
    let end = frame.target.iter().position(|&b| b == 0).unwrap_or(32);
    std::str::from_utf8(&frame.target[..end]).ok()
}
