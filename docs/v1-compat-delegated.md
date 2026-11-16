# v1 Compat — Delegated Mode API

This document specifies the complete delegated-mode surface of the
tollgate-net `v1_compat` layer: the HTTP API an external session daemon
(tollgate-auth's sessiond) uses when it fronts a TollGate gateway and
manages devices on the gateway's behalf.

Delegated mode differs from the portal (captive-device) flow in one
decisive way: **the caller is not the device.** The gateway therefore
identifies the paying/reporting target by MAC — explicitly via the
`X-TollGate-MAC` header or the request path — and never maps the
caller's own IP onto the device.

Two HTTP surfaces are involved, both served on the main listener
(`listen` in `tollgate.yaml`, the same router that carries the Go-v1
wire protocol):

| Surface | Base | Auth | Purpose |
|---|---|---|---|
| Session REST API | `/v1/sessions/...` | `X-API-Key` (GET is unauthenticated — see below) | session queries, delegated bootstrap/top-up, usage reports, deletion |
| Go-v1 wire endpoints | `/` and `/usage` | none (identity via MAC/IP) | delegated payment (`POST /`) and usage polling (`GET /usage`) |

Source of truth for everything below (all behavior verified against
these files):

| Topic | Source |
|---|---|
| Session REST API | `crates/tollgate-net/src/v1_compat/session_api.rs` |
| `X-TollGate-MAC` header, `POST /`, `GET /usage` | `crates/tollgate-net/src/v1_compat/handlers.rs` |
| Crash-recovery journal + NUT-07 reconciliation | `crates/tollgate-net/src/v1_compat/reconcile.rs` |
| Router wiring, `/v1` mount, startup block | `crates/tollgate-net/src/server.rs`, `crates/tollgate-net/src/main.rs` |
| Allotment pricing, kind 1022 event | `crates/tollgate-net/src/v1_compat/merchant.rs` |
| Config parsing | `crates/tollgate-net/src/config.rs` |
| Session store | `crates/tollgate-net/src/v1_compat/session_store.rs` |

---

## 1. The `/v1/sessions` REST API

Mounted under `/v1` on the main router (`server.rs`:
`build_session_router` nested at `/v1`; the m2 lineage nests here and
the tollgate-auth sessiond client calls `/v1/sessions/*`).

### Authentication model

- **`GET /v1/sessions/{mac}` is unauthenticated by contract** (m2 +
  tollgate-auth client behavior): the consumer identifies the session
  via the path MAC and sends no API key on reads. The handler performs
  no key check at all.
- **Every mutating endpoint requires `X-API-Key`**, compared against
  the `TOLLGATE_API_KEY` environment variable. The key is read once at
  router construction (`SessionApiState::api_key`); changing the env
  var later has no effect until restart.
- **Fail-closed when unset:** if `TOLLGATE_API_KEY` is not set at
  construction, `is_authorized` rejects every request — all mutating
  endpoints answer `401 {"error":"unauthorized"}`. There is no
  open-plugin mode.

### MAC normalization

All MACs — path parameters and body fields alike — are normalized
before use: lowercased, with `-` and `.` separators rewritten to `:`
(`normalize_mac` in `session_api.rs`; e.g. `AA-BB-CC-DD-EE-FF` →
`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF` → `aabb:ccdd:eeff`). The
normalized colon form is the session-store key and the
`session_id` returned in responses.

### Endpoints

| Method + path | Auth | Request | Success | Errors |
|---|---|---|---|---|
| `GET /v1/sessions/{mac}` | none | — | `200` `SessionResponse` | `404` `{"error":"session not found","session_id":...}` |
| `POST /v1/sessions/bootstrap` | `X-API-Key` | `{"access_method","token","mac","metric"?}` | `200` `SessionResponse` | `400` metric mismatch, `401`, `402` token rejected |
| `POST /v1/sessions/{mac}/usage` | `X-API-Key` | `{"source"?,"input_octets"?,"output_octets"?,"session_time"?,"timestamp"?}` | `200` updated `SessionResponse` | `401`, `404` no such session |
| `POST /v1/sessions/{mac}/topups` | `X-API-Key` | `{"token"}` | `200` `SessionResponse` | `401`, `402` token rejected |
| `DELETE /v1/sessions/{mac}` | `X-API-Key` | — | `200` `{"deleted":true,"session_id":...}` | `401`, `404` |

All request-body fields not marked `?` are required by the JSON
deserializers (`access_method`, `token`, `mac` on bootstrap; `token`
on topups). Store failures answer `500 {"error":"session store error: ..."}`.

**`POST /v1/sessions/bootstrap`** — creates (or extends) a session for
`mac` by redeeming `token`:

1. `metric`, when present, must equal the server's configured metric.
   A mismatch is rejected `400` (`"unsupported metric '...': this
   TollGate meters in '<configured>'"`) **before any redemption
   attempt** — otherwise the caller would buy allotment at the
   configured step size but be metered in the requested units.
2. The token is redeemed through the mainline driver (see
   [Crash recovery](#3-crash-recovery-nut-07-reconciliation) for the
   journal bracketing). A rejected token answers
   `402 {"error":"payment rejected: <reason>","session_id":...}` and
   creates nothing.
3. On success the allotment (mint pricing × step size, see
   [Configuration](#5-configuration)) is added to the session store
   and the updated `SessionResponse` is returned.

**`POST /v1/sessions/{mac}/usage`** — records an external usage
snapshot (absent octet/time fields default to `0`, `reported_at` is
set server-side). The snapshot never overwrites the allotment; for
non-time metrics `remaining_quota` is computed from the reported
`input_octets + output_octets`.

**`POST /v1/sessions/{mac}/topups`** — same redemption path as
bootstrap (no metric negotiation), then `add_allotment`. Top-ups are
additive: for `milliseconds` sessions a renewal preserves already
consumed wall-clock time (`apply_renewal` shifts `start_time` so
consumed milliseconds are not refunded); for other metrics
`start_time` simply resets.

### `SessionResponse`

```json
{
  "session_id": "02:00:00:00:00:01",
  "access_level": "active",
  "allotment": 1320000,
  "remaining_quota": 1312000,
  "metric": "milliseconds",
  "next_checkin_ms": 5000,
  "is_final": false,
  "created_at": 1794845679
}
```

(The example above is a real verified response from the live surface.)

| Field | Meaning |
|---|---|
| `session_id` | The normalized colon-form MAC (the store key). |
| `access_level` | `"active"` or `"suspended"` — `"suspended"` when remaining ≤ 0. |
| `allotment` | Total granted units of `metric`. |
| `remaining_quota` | `allotment` minus consumption: elapsed wall-clock milliseconds for `milliseconds` sessions; reported `input_octets + output_octets` otherwise. |
| `metric` | The session's metering unit (`"milliseconds"`, `"bytes"`, ...). |
| `next_checkin_ms` | Fixed `5000` — the check-in interval the consumer schedules against (m2 contract). |
| `is_final` | `true` when remaining ≤ 0. |
| `created_at` | Unix seconds — the session's (renewal-adjusted) start time. |
| `last_usage_update` | Server timestamp of the last external usage report; **omitted from the JSON entirely while no usage has been reported.** |

---

## 2. `X-TollGate-MAC` header (delegated payment and usage)

Delegated callers identify the target device with the
`X-TollGate-MAC` header on the Go-v1 wire endpoints
(`handlers.rs: header_mac`). The header value is trimmed, must be
non-empty, and is normalized exactly like path MACs (lowercase,
`-`/`.` → `:`). When present it **overrides IP-based MAC resolution**
on both routes below.

### `POST /` (payment)

Body is a raw Cashu token string (V3 `cashuA...` or V4 `cashuB...`,
optionally JSON `{"token": "..."}`).

- With the header set, the peer is derived from the MAC
  (`adapter::mac_to_peer_hex`) and the admission path receives **no
  IP at all** — the caller's own IP is deliberately not resolved or
  mapped as the device's. Without the header, the caller's IP is
  resolved to a MAC (DHCP leases) and an unresolvable IP is rejected
  `400 {"error":"could not resolve client identity"}`.
- The payment is journaled and redeemed through the driver (same
  ambiguous-outcome bracketing as the session API — portal entries
  leave the journal's `mac` unset).
- On success, header-identified payments credit **both ledgers**: the
  driver peer balance (the money authority) and the session store
  (the delegated ledger tollgate-auth reads via
  `GET /v1/sessions/{mac}`) with the same allotment.
- The `200` response body is a signed **Nostr kind 1022 session
  event** (see the tag list in
  [section 4](#4-the-tollgate-auth-consumer-contract-matrix)).
- A rejected token answers `402` with a kind 21023 notice event
  (`payment_rejected` + reason).

### `GET /usage`

Plain-text Go-v1 contract, not JSON: `"<consumed>/<remaining>"`, or
`"-1/-1"` (HTTP 200) when the MAC cannot be resolved or no session
exists — clients parse "no session" from `-1/-1`, not an error. With
the header set, the queried peer is the header MAC instead of the
IP-resolved one.

---

## 3. Crash recovery (NUT-07 reconciliation)

The danger window: the mint marks proofs spent during redemption
(point of no return), and the process can die before the session is
granted and the HTTP response sent. The customer's token would be
consumed with nothing granted. The `reconcile.rs` journal closes this
gap (the Go v1 ambiguous-outcome pattern, simplified to a file
journal at `/var/lib/tollgate/pending-receives.jsonl`):

1. **Journal before receive.** Every redemption — session-API
   bootstrap/topup (`session_api.rs: redeem`) and portal/delegated
   `POST /` (`handlers.rs: handle_post_payment`) — appends a
   `PendingReceive { token, peer_hex, timestamp, mac? }` line before
   touching the mint. Delegated receives set `mac` (the colon-form
   store key needed to credit the owed session later); portal
   receives leave it unset.
2. **Complete after the credit lands.** A rejected token's fate is
   known (nothing consumed by us) and its entry is removed at once. An
   accepted session-API receive keeps its entry until the session store
   is credited — only then is it removed. Completing before the credit
   would make a crash between the two unrecoverable: token spent,
   journal empty, no session (the chaos-reproduced lost-credit window).
3. **Reconcile at startup** (`main.rs`, gated on the `v1-compat`
   feature): each surviving journaled token is checked at its mint
   via NUT-07 `POST {mint}/v1/checkstate`, keying state by
   `Y = hash_to_curve(secret)` — the C value never matches mint
   state, so any other derivation silently reads spent tokens as
   unspent. Outcomes:
    - **All proofs SPENT** → the receive succeeded but the session was
      never granted → `grant_owed_receive`: the driver credits the
      peer (amount × 1000, millisat balance), and — for delegated
      entries (`mac` set) — the session store is credited with the
      allotment the mint pricing implies. A zero owed amount is
      refused outright. The grant is **token-idempotent** (see
      below), and the entry is removed only after the grant lands.
    - **UNSPENT** → the receive never completed; the customer keeps
      the token. Entry dropped.
    - **Partially spent / mint unreachable / parse error** →
      indeterminate; the entry stays journaled for the next boot.

Safety properties:

- The whole startup pass runs under a **30-second timeout**; on
  timeout the journal is left intact and retried at the next boot
  (non-fatal).
- **Owed and indeterminate entries stay journaled until their grant
  lands** — the debt record is never lost, so a transient store/mint
  error or a crash between grant and completion cannot silently
  swallow an owed session.
- **Recovery crediting is idempotent per token**: the session store's
  `credit_for_token` claims the token and adds the allotment in one
  atomic step (the SQLite backend records claimed tokens in a
  `credited_tokens` table). A journal entry re-derived after a crash
  between grant and completion is therefore a no-op, not a double
  credit — the property that makes "keep the entry until granted"
  safe with a persistent store.
- Journal robustness: garbage lines are skipped on read, and rewrites
  go through a temp file + rename.

---

## 4. The tollgate-auth consumer contract matrix

tollgate-auth's Go sessiond is the reference delegated consumer. Its
client surface maps onto this server as follows (all three verified
live against the testnut fakewallet mint, 2026-11-16):

| sessiond call | HTTP request | Server behavior |
|---|---|---|
| `Bootstrap` | `POST /` with `X-TollGate-MAC: <device mac>` and the raw token as body | Redeems via the driver, credits driver + session store, answers `200` with the kind 1022 session event |
| `GetSession` | `GET /v1/sessions/{mac}` (no API key) | `200` `SessionResponse` / `404` |
| `ReportUsage` | `POST /v1/sessions/{mac}/usage` with `X-API-Key` and the usage JSON | Records the snapshot, answers `200` with the updated `SessionResponse` |

The `Bootstrap` response is a signed Nostr **kind 1022** session
event carrying the grant as tags (`merchant.rs: build_session_event`):
`p` (customer identifier), `allotment`, `metric`, `device-identifier`
(`mac` + address), `start-time`, `amount_sat`, `token_type`, and —
when computable — `effective_rate` (`(allotment / 1000) / amount_sat`).

---

## 5. Configuration

Delegated mode is configured by the top-level `mints` list plus the
`v1_compat` block in `tollgate.yaml`:

```yaml
mints:
  - https://testnut.cashu.space

v1_compat:
  metric: milliseconds        # metering unit; empty → "milliseconds"
  step_size: 60000            # units of metric sold per pricing step; 0 → 60000
  accepted_mints:             # per-mint pricing (first entry also keys
    - url: https://testnut.cashu.space   # owed-receive allotment)
      price_per_step: 1       # sats per step
      unit: sat
      min_steps: 1            # payments below this many steps are rejected
  wallet_seed: "<per-deployment hex seed>"   # optional; persistent CDK wallet
```

Allotment math (`merchant.rs: calculate_allotment`):
`steps = amount_sat / price_per_step`; `steps < min_steps` is an
error; otherwise `allotment = steps × step_size`. (Accepted-mint
defaults when unset: `http://localhost:3338`, 1 sat/step, min 1 step.)
Further `v1_compat` knobs exist (`nostr_secret_key`,
`trust_proxy_headers`, `profit_share`); the delegated surface itself
depends only on the keys above plus the `TOLLGATE_API_KEY` env var.

### Session store durability

Two backends, selected by the `v1_compat.session_store_path` config
knob:

- **Unset (default)** — in-memory (`InMemorySessionStore`, a tokio
  mutex-guarded HashMap). Sessions do **not** survive restarts, with
  one exception: owed delegated receives recovered by the startup
  journal (section 3) re-credit the store.
- **Set to a file path** — persistent SQLite (`SqliteSessionStore`,
  `sessions(mac TEXT PRIMARY KEY, session_json TEXT)` rows plus a
  `credited_tokens` dedupe table backing idempotent recovery, section
  3). Sessions survive restarts. If the configured database cannot be
  opened the server logs an error and degrades to the in-memory store
  rather than failing to start.

The SQLite backend links `libsqlite3-sys 0.28`, unified with
cdk-sqlite via a `rusqlite =0.31` pin in `crates/tollgate-net` —
the version conflict that previously deferred this store was
resolved by that pin (bump it casually and the two sqlite3 native
links diverge again).
