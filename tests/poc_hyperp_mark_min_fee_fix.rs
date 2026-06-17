#![allow(dead_code, unused_imports, clippy::field_reassign_with_default)]
//! Regression test for the fix to the Hyperp `mark_min_fee == 0` liveness-spoof
//! (internally "F2", `scripts/security.md:448-487`).
//!
//! Fix: `handle_init_market` now rejects
//!   `is_hyperp && permissionless_resolve_stale_slots > 0 && mark_min_fee == 0`
//! with `InvalidConfigParam`, so the spoofable configuration can no longer be
//! created. (`mark_min_fee` and `permissionless_resolve_stale_slots` are set-once
//! at init — not mutable via UpdateConfig or any Set* handler — so the single
//! gate is complete.)
//!
//! These tests prove the gate is precise — it rejects EXACTLY the vulnerable
//! tuple and accepts every safe variant (flip any one conjunct → accepted).

mod common;
#[allow(unused_imports)]
use common::*;

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    sysvar,
    transaction::Transaction,
};

const INIT_MARK_E6: u64 = 138_000_000;

/// Hyperp InitMarket wire bytes, built from the harness's known-good
/// `encode_init_market_hyperp` with the extended tail patched: perm_resolve,
/// mark_min_fee, and force_close (see the InitMarket decoder, src:2158-2228;
/// tail fields: perm_resolve @10, mark_min_fee @50, force_close @58).
fn encode_init_hyperp(admin: &Pubkey, mint: &Pubkey, perm_resolve: u64, mark_min_fee: u64) -> Vec<u8> {
    let mut d = encode_init_market_hyperp(admin, mint, INIT_MARK_E6);
    let tail = d.len() - 66;
    d[tail + 10..tail + 18].copy_from_slice(&perm_resolve.to_le_bytes());
    d[tail + 50..tail + 58].copy_from_slice(&mark_min_fee.to_le_bytes());
    d[tail + 58..tail + 66].copy_from_slice(&50u64.to_le_bytes());
    d
}

fn try_init_hyperp(env: &mut TradeCpiTestEnv, perm_resolve: u64, mark_min_fee: u64) -> Result<(), String> {
    let admin = env.payer.insecure_clone();
    let ix = Instruction {
        program_id: env.program_id,
        accounts: vec![
            AccountMeta::new(admin.pubkey(), true),
            AccountMeta::new(env.slab, false),
            AccountMeta::new_readonly(env.mint, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
            AccountMeta::new_readonly(sysvar::rent::ID, false),
            AccountMeta::new_readonly(env.pyth_index, false),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ],
        data: encode_init_hyperp(&admin.pubkey(), &env.mint, perm_resolve, mark_min_fee),
    };
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix(), ix],
        Some(&admin.pubkey()),
        &[&admin],
        env.svm.latest_blockhash(),
    );
    env.svm
        .send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e))
}

// FIX: the vulnerable tuple (Hyperp + perm-resolve + mark_min_fee==0) is rejected.
// PercolatorError::InvalidConfigParam == Custom(26).
#[test]
fn vulnerable_hyperp_config_is_now_rejected() {
    let mut env = TradeCpiTestEnv::new();
    let r = try_init_hyperp(&mut env, 50, 0);
    assert!(
        r.as_ref().err().is_some_and(|e| e.contains("Custom(26)")),
        "FIX: Hyperp + perm_resolve>0 + mark_min_fee==0 must be rejected with \
         InvalidConfigParam (Custom(26)); got {r:?}"
    );
}

// NO OVER-REJECTION: a nonzero mark_min_fee (the safe config) is accepted.
#[test]
fn hyperp_with_nonzero_mark_min_fee_is_accepted() {
    let mut env = TradeCpiTestEnv::new();
    assert!(
        try_init_hyperp(&mut env, 50, 1).is_ok(),
        "Hyperp + perm_resolve>0 + mark_min_fee>0 must still be accepted"
    );
}

// NO OVER-REJECTION: admin-resolve-only Hyperp (perm_resolve==0) keeps
// mark_min_fee==0 (F2 carve-out — no permissionless gate to spoof).
#[test]
fn admin_resolve_only_hyperp_keeps_zero_mark_min_fee() {
    let mut env = TradeCpiTestEnv::new();
    assert!(
        try_init_hyperp(&mut env, 0, 0).is_ok(),
        "Hyperp + perm_resolve==0 + mark_min_fee==0 must still be accepted"
    );
}

// NO OVER-REJECTION: a non-Hyperp market with mark_min_fee==0 is unaffected
// (its liveness keys on last_good_oracle_slot, not trade-driven). The harness's
// `init_market` builds a non-Hyperp market (real feed) with mark_min_fee==0 and
// perm_resolve>0; it must still initialize.
#[test]
fn non_hyperp_with_zero_mark_min_fee_is_unaffected() {
    let mut env = TradeCpiTestEnv::new();
    env.init_market(); // panics on failure -> success is the assertion
    assert!(env.is_market_resolved() == false, "market initialized (Live), not rejected");
}
