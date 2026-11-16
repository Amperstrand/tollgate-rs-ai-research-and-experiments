//! NUT-07 startup reconciliation for ambiguous payment receives.
//!
//! When a customer POSTs a Cashu token, the server verifies + receives it
//! (the mint marks the proofs spent — point of no return), then creates a
//! session and returns HTTP 200. If the process crashes between the receive
//! and the response, the customer's token is consumed but they got nothing.
//!
//! This module closes that gap:
//! 1. BEFORE the receive, journal the token + peer to a pending file.
//! 2. AFTER successful admission, remove the entry.
//! 3. On startup, read pending entries and check NUT-07 for each token:
//!    - SPENT → the receive succeeded but the session wasn't granted →
//!      grant it now (the customer is owed access).
//!    - UNSPENT → the receive never completed → remove the entry (the
//!      customer keeps their token; nothing was consumed).
//!
//! Pattern: the Go v1's ambiguous-outcome reconciliation (#502), simplified
//! to a file-based journal. The Rust basic-rust's payment_journal.rs is the
//! full-durability version (fsync'd appends); this is the lightweight
//! compat-layer version.

use std::path::Path;

/// Default journal location (alongside the wallet DB).
pub const DEFAULT_PENDING_RECEIVES_FILE: &str = "/var/lib/tollgate/pending-receives.jsonl";

/// A pending receive entry — written before the receive, removed after.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingReceive {
    /// The full Cashu token string (needed for NUT-07 proof extraction).
    pub token: String,
    /// The peer identity (MAC-derived hex) for session attribution.
    pub peer_hex: String,
    /// Unix timestamp when the intent was journaled.
    pub timestamp: i64,
    /// Colon-form MAC for delegated (session-API) receives — the session
    /// store key needed to credit owed sessions at startup. Portal
    /// receives leave this unset.
    #[serde(default)]
    pub mac: Option<String>,
}

/// Append a pending-receive intent to the journal (BEFORE the receive).
pub fn journal_pending_receive(journal_path: &Path, entry: &PendingReceive) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = journal_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path)?;
    writeln!(f, "{line}")
}

/// Remove a completed entry from the journal (AFTER successful admission).
/// Rewrites the file without the matching token.
pub fn complete_pending_receive(journal_path: &Path, token: &str) -> std::io::Result<()> {
    let entries = read_journal(journal_path)?;
    let remaining: Vec<&PendingReceive> = entries.iter().filter(|e| e.token != token).collect();
    rewrite_journal(journal_path, &remaining)
}

/// Read all pending entries from the journal.
fn read_journal(journal_path: &Path) -> std::io::Result<Vec<PendingReceive>> {
    let contents = match std::fs::read_to_string(journal_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Rewrite the journal with only the given entries.
fn rewrite_journal(journal_path: &Path, entries: &[&PendingReceive]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = journal_path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    for e in entries {
        writeln!(
            f,
            "{}",
            serde_json::to_string(e)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        )?;
    }
    drop(f);
    std::fs::rename(&tmp, journal_path)
}

/// Outcome of reconciling a single pending receive.
#[derive(Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The token was spent at the mint — the receive succeeded but the
    /// session wasn't granted. The customer is owed access.
    OwedSession { peer_hex: String, amount_sat: u64 },
    /// The token was unspent — the receive never completed. The customer
    /// keeps their token; nothing was consumed.
    NotConsumed,
    /// Could not determine (mint unreachable, parse error). The entry
    /// stays in the journal for a later retry.
    Indeterminate(String),
}

/// Check a single token against NUT-07 to determine whether it was consumed.
pub async fn check_token_consumed(
    _mint_base_url: &str,
    token_str: &str,
) -> Result<ReconcileOutcome, String> {
    let token: cashu::nuts::Token = token_str.parse().map_err(|e| format!("token parse: {e}"))?;

    let mint_url = token
        .mint_url()
        .map_err(|e| format!("no mint URL: {e}"))?
        .to_string();
    let mint_base = mint_url.trim_end_matches('/');

    let ys = extract_proof_ys(&token);
    if ys.is_empty() {
        return Ok(ReconcileOutcome::NotConsumed);
    }

    let url = format!("{mint_base}/v1/checkstate");
    super::ssrf::validate_url_no_ssrf(&url).map_err(|e| e.to_string())?;
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!({ "Ys": ys }))
        .send()
        .await
        .map_err(|e| format!("checkstate request: {e}"))?
        .error_for_status()
        .map_err(|e| format!("checkstate HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("checkstate parse: {e}"))?;

    let states = resp["states"]
        .as_array()
        .ok_or("missing 'states' in response")?;

    let all_spent = states.iter().all(|s| s["state"].as_str() == Some("SPENT"));
    let any_spent = states.iter().any(|s| s["state"].as_str() == Some("SPENT"));

    if all_spent {
        let amount: u64 = token.value().map_err(|e| format!("value sum: {e}"))?.into();
        Ok(ReconcileOutcome::OwedSession {
            peer_hex: String::new(), // filled by the caller
            amount_sat: amount,
        })
    } else if any_spent {
        // Partially spent is ambiguous — treat as indeterminate
        Ok(ReconcileOutcome::Indeterminate(
            "partially spent proofs".to_owned(),
        ))
    } else {
        Ok(ReconcileOutcome::NotConsumed)
    }
}

/// Extract proof Y-values from a token for NUT-07 checkstate.
fn extract_proof_ys(token: &cashu::nuts::Token) -> Vec<String> {
    use cashu::nuts::Token;
    // NUT-07 keys proof state by Y = hash_to_curve(secret). The C
    // value is the unblinded signature — sending it as a Y never
    // matches mint state, so every spent token read as unspent.
    let secrets: Vec<&[u8]> = match token {
        Token::TokenV3(t) => t
            .token
            .iter()
            .flat_map(|e| e.proofs.iter().map(|p| p.secret.as_bytes()))
            .collect(),
        Token::TokenV4(t) => t
            .token
            .iter()
            .flat_map(|e| e.proofs.iter().map(|p| p.secret.as_bytes()))
            .collect(),
    };
    secrets
        .iter()
        .filter_map(|s| cashu::dhke::hash_to_curve(s).ok())
        .map(|y| y.to_string())
        .collect()
}

/// An owed session recovered from the journal: the token was consumed at
/// the mint (NUT-07 SPENT) but no session was granted before the crash.
#[derive(Debug, Clone)]
pub struct OwedReceive {
    pub entry: PendingReceive,
    pub amount_sat: u64,
}

/// Reconcile all pending receives on startup. Returns the entries that are
/// owed sessions (the caller grants them) and removes resolved entries.
pub async fn reconcile_pending_receives(journal_path: &Path) -> std::io::Result<Vec<OwedReceive>> {
    let entries = read_journal(journal_path)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    tracing::info!(
        count = entries.len(),
        "reconciling pending receives from journal"
    );

    let mut owed = Vec::new();
    let mut unresolved = Vec::new();

    for entry in &entries {
        match check_token_consumed("", &entry.token).await {
            Ok(ReconcileOutcome::OwedSession { amount_sat, .. }) => {
                tracing::warn!(
                    peer = %entry.peer_hex,
                    amount_sat,
                    "pending receive was consumed but session not granted — owed"
                );
                owed.push(OwedReceive {
                    entry: entry.clone(),
                    amount_sat,
                });
            }
            Ok(ReconcileOutcome::NotConsumed) => {
                tracing::info!(
                    peer = %entry.peer_hex,
                    "pending receive was NOT consumed — removing from journal"
                );
                // Entry is dropped (not added to unresolved)
            }
            Ok(ReconcileOutcome::Indeterminate(reason)) => {
                tracing::warn!(
                    peer = %entry.peer_hex,
                    reason,
                    "pending receive indeterminate — keeping for retry"
                );
                unresolved.push(entry);
            }
            Err(e) => {
                tracing::warn!(
                    peer = %entry.peer_hex,
                    error = %e,
                    "pending receive check failed — keeping for retry"
                );
                unresolved.push(entry);
            }
        }
    }

    // Rewrite the journal with only unresolved entries
    let refs: Vec<&PendingReceive> = unresolved.as_slice().to_vec();
    rewrite_journal(journal_path, &refs)?;

    Ok(owed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_entry_without_mac_parses() {
        let line = r#"{"token":"cashuAtest","peer_hex":"aabb","timestamp":1}"#;
        let entry: PendingReceive = serde_json::from_str(line).expect("legacy entry parses");
        assert_eq!(entry.mac, None);
    }

    #[test]
    fn ys_derive_from_secrets_not_c_values() {
        // A real V4 token fixture: NUT-07 keys state by
        // hash_to_curve(secret); extracting the C values instead would
        // make every spent token read as unspent (the live-verified bug).
        let token: cashu::nuts::Token = r#"cashuBo2FteBtodHRwczovL3Rlc3RudXQuY2FzaHUuc3BhY2VhdWNzYXRhdIGiYWlIAXvGBKqgjpthcIOkYWECYXN4QDExOGYxZDBhYTFmNGE3N2I3ZTEwOTU4NTgyZDA0ZGJjMmYwZjRlYmQzODQzMzE1ODZlNGQ2NWMxZjJmOGM1ZWFhY1ghAr-JiKoUH_0LUbTLTED3Q3XEwh7k58HIdwik_uJrtqEGYWSjYWVYIH9xweu7AKIacaN1--TwwEbaZN43Z1Y_HV5D93HMJ-pgYXNYIIPOv0xCJl__Wu09EFWGwTm_SxHEz7YwtAyoiUK0dY7JYXJYIC9U3ORWnr8G85YvNVoDQ3YuM22S737_AW1ffSMn0tyfpGFhEGFzeEBjYjljMzg3YTg3MjVkOWQ1YjBhNjhhYzEzYzdmMjM2MjYzYWM1NDY5NWYwYTIwM2VhMmMwYzEwZDA3NTRlZWE4YWNYIQJqzpWXqIDOvEyldRjzR7Y6Rc0nPMEJZG4UexA2r_iukmFko2FlWCDbke1r-0t_v-N8Aj_Vz7Juq0DGzSa8lcFYmikBNsB8zWFzWCDxEmFGmhSjpxlITzLouQbjZO7P37tfQ7U9eiqQA9VlW2FyWCCXJDamu9hdLNCwkChZvMd7F-469zPSYZAZ2XVBlA3OPKRhYQRhc3hAMjNiYTdhM2U2ZTcxMWRhNTAzYTJjNWMzMTY0NTNlMWVkYmVmODE5YzI1NTQ4ZWU4NmY5OGJkNmYxMGQxMzI0M2FjWCED_GAH6U3v0Xf71jwIg2aYD68ap5_H2s94brzjuicBZK1hZKNhZVggsCwoP9muOsDFJ0qQMhifjeNhrDhEtKpAIHKPE3beqVJhc1ggDbq0T8y70d349DE7ws-eH4fEInAmgfEkPqrKCiTxDu5hclggKJJ1BkdRi14hasvJC5vir4LSs_OdexXmMNWGgFgmb-M="#.parse().expect("fixture parses");
        let ys = extract_proof_ys(&token);
        let secrets: Vec<&[u8]> = match &token {
            cashu::nuts::Token::TokenV3(t) => t
                .token
                .iter()
                .flat_map(|e| e.proofs.iter().map(|p| p.secret.as_bytes()))
                .collect(),
            cashu::nuts::Token::TokenV4(t) => t
                .token
                .iter()
                .flat_map(|e| e.proofs.iter().map(|p| p.secret.as_bytes()))
                .collect(),
        };
        let c_values: Vec<String> = match &token {
            cashu::nuts::Token::TokenV3(t) => t
                .token
                .iter()
                .flat_map(|e| e.proofs.iter().map(|p| p.c.to_string()))
                .collect(),
            cashu::nuts::Token::TokenV4(t) => t
                .token
                .iter()
                .flat_map(|e| e.proofs.iter().map(|p| p.c.to_string()))
                .collect(),
        };
        let expected: Vec<String> = secrets
            .iter()
            .filter_map(|s| cashu::dhke::hash_to_curve(s).ok().map(|y| y.to_string()))
            .collect();
        assert_eq!(ys, expected, "checkstate Ys must be hash_to_curve(secret)");
        assert_ne!(ys, c_values, "regression: extracting C values again");
    }

    #[test]
    fn journal_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tg-recon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending.jsonl");

        let entry = PendingReceive {
            token: "cashuBtest...".to_owned(),
            peer_hex: "aabbcc".to_owned(),
            timestamp: 12345,
            mac: None,
        };
        journal_pending_receive(&path, &entry).unwrap();
        let read = read_journal(&path).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].token, entry.token);

        complete_pending_receive(&path, &entry.token).unwrap();
        let after = read_journal(&path).unwrap();
        assert_eq!(after.len(), 0, "completed entry should be removed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn journal_missing_file_is_empty() {
        let path = Path::new("/nonexistent/no-such-file.jsonl");
        assert!(read_journal(path).unwrap().is_empty());
    }

    #[test]
    fn journal_survives_partial_writes() {
        let dir = std::env::temp_dir().join(format!("tg-recon-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending.jsonl");
        std::fs::write(
            &path,
            "not-json\n{\"token\":\"ok\",\"peer_hex\":\"aa\",\"timestamp\":1}\n\n",
        )
        .unwrap();

        let read = read_journal(&path).unwrap();
        assert_eq!(read.len(), 1, "garbage lines are skipped, valid entry kept");
        assert_eq!(read[0].token, "ok");

        std::fs::remove_dir_all(&dir).ok();
    }
}
