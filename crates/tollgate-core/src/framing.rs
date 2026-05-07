//! 2-byte LE length-prefix framing for TollGate HTTP polling transport.
//!
//! Each message in a frame body is prefixed with a 2-byte little-endian length.
//! Multiple messages can appear in one frame body. An empty body (0 bytes) is
//! valid and means zero messages.
//!
//! Wire layout:
//!
//! ```text
//! [len0_lo, len0_hi, msg0..., len1_lo, len1_hi, msg1..., ...]
//! ```

use crate::protocol::Message;

/// Errors that can occur when decoding a framed byte buffer.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("truncated length prefix at offset {offset}")]
    TruncatedLength { offset: usize },

    #[error("truncated message at offset {offset}: expected {expected} bytes, have {available}")]
    TruncatedMessage {
        offset: usize,
        expected: usize,
        available: usize,
    },

    #[error("CBOR decode error at offset {offset}: {source}")]
    DecodeError {
        offset: usize,
        source: minicbor::decode::Error,
    },
}

/// Encode multiple messages into a framed byte buffer.
///
/// Each message is CBOR-encoded and prefixed with a 2-byte LE length.
/// Empty input produces empty output.
///
/// # Errors
///
/// Returns an error if CBOR encoding of any message fails.
///
/// # Panics
///
/// Panics if any CBOR-encoded message exceeds 64 KiB (the 2-byte length prefix limit).
pub fn encode_frame(messages: &[Message]) -> Result<Vec<u8>, minicbor::encode::Error<std::convert::Infallible>> {
    let mut buf = Vec::new();
    for msg in messages {
        let msg_bytes = minicbor::to_vec(msg)?;
        let len = u16::try_from(msg_bytes.len()).expect("CBOR message exceeds 64KiB framing limit");
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&msg_bytes);
    }
    Ok(buf)
}

/// Decode multiple messages from a framed byte buffer.
///
/// Returns decoded messages. Rejects truncated or corrupted input.
///
/// # Errors
///
/// Returns [`FrameError::TruncatedLength`] if the length prefix is incomplete,
/// [`FrameError::TruncatedMessage`] if a message is shorter than its length prefix claims,
/// or [`FrameError::DecodeError`] if CBOR decoding fails.
pub fn decode_frame(data: &[u8]) -> Result<Vec<Message>, FrameError> {
    let mut messages = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        if offset + 2 > data.len() {
            return Err(FrameError::TruncatedLength { offset });
        }
        let len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        if offset + len > data.len() {
            return Err(FrameError::TruncatedMessage {
                offset,
                expected: len,
                available: data.len() - offset,
            });
        }

        let msg = minicbor::decode(&data[offset..offset + len])
            .map_err(|e| FrameError::DecodeError {
                offset,
                source: e,
            })?;
        messages.push(msg);
        offset += len;
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;

    #[test]
    fn empty_frame() {
        let encoded = encode_frame(&[]).unwrap();
        assert!(encoded.is_empty());
        let decoded = decode_frame(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_message_frame() {
        let msg = Message::Disconnect(Disconnect {
            msg_type: 14,
            reason_code: ReasonCode::VersionUnsupported,
        });
        let encoded = encode_frame(std::slice::from_ref(&msg)).unwrap();
        let decoded = decode_frame(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], msg);
    }

    #[test]
    fn three_messages_frame() {
        let msgs = vec![
            Message::Disconnect(Disconnect {
                msg_type: 14,
                reason_code: ReasonCode::Other,
            }),
            Message::BalanceAck(BalanceAck {
                msg_type: 6,
                channel_id: Hash32([0xAA; 32]),
                accepted_balance: 100,
            }),
            Message::CloseAck(CloseAck {
                msg_type: 12,
                channel_id: Hash32([0xBB; 32]),
                accepted_balance: 200,
            }),
        ];
        let encoded = encode_frame(&msgs).unwrap();
        let decoded = decode_frame(&encoded).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded, msgs);
    }

    #[test]
    fn truncated_length() {
        let result = decode_frame(&[0x00]);
        assert!(matches!(
            result,
            Err(FrameError::TruncatedLength { offset: 0 })
        ));
    }

    #[test]
    fn truncated_message() {
        let len_bytes = 100u16.to_le_bytes();
        let result = decode_frame(&[len_bytes[0], len_bytes[1], 0x00]);
        assert!(matches!(result, Err(FrameError::TruncatedMessage { .. })));
    }

    #[test]
    fn corrupted_cbor() {
        let len_bytes = 5u16.to_le_bytes();
        let result = decode_frame(&[
            len_bytes[0],
            len_bytes[1],
            0xFF,
            0xFF,
            0xFF,
            0xFF,
            0xFF,
        ]);
        assert!(matches!(result, Err(FrameError::DecodeError { .. })));
    }

    #[test]
    fn frame_preserves_message_boundaries() {
        let msg1 = Message::Disconnect(Disconnect {
            msg_type: 14,
            reason_code: ReasonCode::Other,
        });
        let msg2 = Message::Disconnect(Disconnect {
            msg_type: 14,
            reason_code: ReasonCode::PriceTooHigh,
        });
        let msg1_bytes = minicbor::to_vec(&msg1).unwrap();
        let encoded = encode_frame(&[msg1, msg2]).unwrap();
        let len1 = u16::from_le_bytes([encoded[0], encoded[1]]) as usize;
        assert_eq!(len1, msg1_bytes.len());
    }
}
