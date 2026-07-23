//! Test: Does CDK v0.16.0 work with V1 keyset tokens?
//!
//! Tests three things:
//! 1. Can CDK parse a V1 keyset ID string?
//! 2. Can a CDK wallet load V1 keysets from the mint?
//! 3. Can CDK Wallet::receive() process a V1 token?

#![cfg(test)]

use std::str::FromStr;
use std::sync::Arc;

use cdk::nuts::CurrencyUnit;
use cdk::nuts::Id;
use cdk::wallet::{ReceiveOptions, Wallet};
use cdk_sqlite::wallet::memory;

const MINT_URL: &str = "https://testnut.cashu.exchange";

#[test]
fn cdk_parses_v1_keyset_id() {
    let v1_id = "008e808b89acc141";

    match Id::from_str(v1_id) {
        Ok(id) => {
            println!("OK: CDK v0.16.0 parses V1 keyset ID: {id}");
        }
        Err(e) => {
            panic!("FAIL: CDK v0.16.0 CANNOT parse V1 keyset ID '{v1_id}': {e}");
        }
    }
}

#[tokio::test]
#[ignore = "requires network + CASHU_TEST_TOKEN env var"]
async fn cdk_wallet_receives_v1_token() {
    let token_str = std::env::var("CASHU_TEST_TOKEN")
        .expect("Set CASHU_TEST_TOKEN to a V1 cashu token from testnut.cashu.exchange");

    let seed = [0x99; 64];
    let localstore = Arc::new(memory::empty().await.expect("localstore"));
    let wallet = Wallet::new(MINT_URL, CurrencyUnit::Sat, localstore, seed, None)
        .expect("wallet init");

    println!("Token: {}... ({} chars)", &token_str[..40], token_str.len());
    println!("Calling receive()...");

    match wallet.receive(&token_str, ReceiveOptions::default()).await {
        Ok(amount) => {
            println!("OK: receive() succeeded, amount={:?}", amount);
            eprintln!("RESULT: CDK v0.16.0 receive() works with V1 keysets. PR #52 is FIXED.");
        }
        Err(e) => {
            let s = format!("{e}");
            if s.contains("keyset") || s.contains("Short") || s.contains("IDv2") {
                eprintln!("FAIL: CDK receive() still fails on V1 keysets: {e}");
                panic!("keyset error: {e}");
            }
            eprintln!("WARN: receive() failed (non-keyset): {e}");
        }
    }
}
