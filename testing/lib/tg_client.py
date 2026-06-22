#!/usr/bin/env python3
"""Minimal TollGate CBOR test client — stdlib only, no pip packages.

Sends CBOR-encoded TollGate messages over HTTP polling transport
(2-byte LE length-prefixed frames to POST /tollgate/v1/exchange).

The upstream `tollgate` client binary only sends Announce and
BootstrapToken, so this script is needed to exercise Disconnect (0x0E)
and Reject (0x0D) end-to-end through the gateway.
"""
import struct
import urllib.request
import urllib.error
import sys
import os


# ---------------------------------------------------------------------------
# Minimal CBOR encoder (RFC 8949 subset — enough for our flat integer-keyed maps).
# ---------------------------------------------------------------------------

def _cbor_head(major, value):
    if value < 24:
        return bytes([major << 5 | value])
    elif value < 256:
        return bytes([major << 5 | 24, value])
    elif value < 65536:
        return bytes([major << 5 | 25]) + struct.pack('>H', value)
    elif value < 2 ** 32:
        return bytes([major << 5 | 26]) + struct.pack('>I', value)
    else:
        return bytes([major << 5 | 27]) + struct.pack('>Q', value)


def cbor_uint(v):
    return _cbor_head(0, v)


def cbor_nint(v):
    return _cbor_head(1, -v - 1)


def cbor_bstr(b):
    return _cbor_head(2, len(b)) + b


def cbor_tstr(s):
    b = s.encode('utf-8')
    return _cbor_head(3, len(b)) + b


def cbor_null():
    return bytes([0xf6])


def cbor_map(pairs):
    body = b''
    for key, encoded_val in pairs:
        body += cbor_uint(key) + encoded_val
    return _cbor_head(5, len(pairs)) + body


# ---------------------------------------------------------------------------
# TollGate message builders.
#
# Field keys follow crates/tollgate-protocol/src/message.rs:
#   0 = message type discriminant (MessageType enum as u8)
#   Announce:  1=version, 2=pubkey(bstr), 3=unit(tstr), 4=capabilities(uint)
#   Disconnect: 1=reason_code(uint)
#   Reject:    1=rejected_type(uint), 2=reason_code(uint), 3=reason_text(tstr|null)
# ---------------------------------------------------------------------------

MSG_ANNOUNCE = 0x00
MSG_REJECT = 0x0D
MSG_DISCONNECT = 0x0E

# Protocol version (matches PROTOCOL_VERSION in the Rust crate).
PROTOCOL_VERSION = 1


def make_announce(pubkey_33bytes, unit="bytes", capabilities=0):
    return cbor_map([
        (0, cbor_uint(MSG_ANNOUNCE)),
        (1, cbor_uint(PROTOCOL_VERSION)),
        (2, cbor_bstr(pubkey_33bytes)),
        (3, cbor_tstr(unit)),
        (4, cbor_uint(capabilities)),
    ])


def make_disconnect(reason_code=0x0E):
    """reason_code defaults to Other (0x0E). See RejectReason in message.rs."""
    return cbor_map([
        (0, cbor_uint(MSG_DISCONNECT)),
        (1, cbor_uint(reason_code)),
    ])


def make_reject(rejected_type=0x01, reason_code=0x01, reason_text=None):
    """rejected_type defaults to PriceSheet (0x01), reason to PriceTooHigh (0x01)."""
    pairs = [
        (0, cbor_uint(MSG_REJECT)),
        (1, cbor_uint(rejected_type)),
        (2, cbor_uint(reason_code)),
        (3, cbor_tstr(reason_text) if reason_text else cbor_null()),
    ]
    return cbor_map(pairs)


def random_pubkey():
    """33 random bytes as a fake pubkey (the gateway doesn't verify the curve)."""
    return os.urandom(33)


# ---------------------------------------------------------------------------
# Wire framing: 2-byte LE length prefix per message.
# ---------------------------------------------------------------------------

def frame(msg_bytes):
    """2-byte LE length prefix + message payload."""
    return struct.pack('<H', len(msg_bytes)) + msg_bytes


def send_exchange(base_url, *messages):
    """POST one or more CBOR messages to the exchange endpoint.

    Returns the raw response bytes (the gateway's framed reply, or b'' on
    empty responses which are valid for Disconnect/Reject).
    """
    body = b''.join(frame(m) for m in messages)
    url = base_url.rstrip('/') + '/tollgate/v1/exchange'
    req = urllib.request.Request(url, data=body, method='POST')
    req.add_header('Content-Type', 'application/cbor')
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        sys.stderr.write(
            f"HTTP {e.code}: {e.read().decode('utf-8', 'replace')}\n"
        )
        raise
    except urllib.error.URLError as e:
        sys.stderr.write(f"Connection failed: {e}\n")
        raise


def announce_and_send(base_url, followup_msg, unit="bytes"):
    """Send Announce + followup message in a single exchange.

    The Announce must precede any other message in an exchange so the
    gateway can establish peer identity (see server.rs).
    """
    pk = random_pubkey()
    return send_exchange(base_url, make_announce(pk, unit), followup_msg)


# ---------------------------------------------------------------------------
# CLI entry point.
# ---------------------------------------------------------------------------

if __name__ == '__main__':
    # CLI: tg_client.py <base_url> <message_type> [reason_code]
    base_url = sys.argv[1] if len(sys.argv) > 1 else "http://gateway:4747"
    msg_type = sys.argv[2] if len(sys.argv) > 2 else "disconnect"

    if msg_type == "disconnect":
        # Default 0x0E = RejectReason::Other
        reason = int(sys.argv[3], 0) if len(sys.argv) > 3 else 0x0E
        msg = make_disconnect(reason)
    elif msg_type == "reject":
        # Default 0x01 = RejectReason::PriceTooHigh
        reason = int(sys.argv[3], 0) if len(sys.argv) > 3 else 0x01
        msg = make_reject(reason_code=reason, reason_text="test rejection")
    else:
        sys.stderr.write(f"Unknown message type: {msg_type}\n")
        sys.exit(1)

    resp = announce_and_send(base_url, msg)
    print(f"Response: {len(resp)} bytes")
    sys.exit(0)
