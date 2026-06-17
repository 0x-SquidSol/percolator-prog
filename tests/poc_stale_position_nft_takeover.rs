#![allow(
    dead_code,
    unused_imports,
    unused_mut,
    clippy::useless_conversion,
    clippy::field_reassign_with_default
)]
//! Regression test (LiteSVM, real BPF) for the stale-position-NFT slot-reuse
//! account-takeover, and its fix.
//!
//! ── The bug (pre-fix) ─────────────────────────────────────────────────────────
//! `TransferPositionOwnership` (src/percolator.rs `handle_transfer_position_ownership`)
//! authorized the caller ONLY against the position-NFT PDA's cached `owner` field
//! and then overwrote `engine.accounts[user_idx].owner`. The NFT PDA is derived
//! purely from (program_id, slab, user_idx) and `CloseAccount` never touches it,
//! so a stale NFT from a prior occupant of a slot could authorize a takeover once
//! the engine reused that slot index for a different account.
//!
//! ── The fix ──────────────────────────────────────────────────────────────────
//! `PositionNftState` now carries the slot's per-materialization `generation`
//! (the `mat_counter` value, stamped at mint from the slab gen-table). Transfer
//! requires `nft.generation == read_account_generation(user_idx)` (the LIVE
//! occupant's generation). A stale NFT carries the prior occupant's generation,
//! which differs after slot reuse, so the takeover is rejected with
//! `EngineUnauthorized`. Legitimate same-materialization transfers still work.
//!
//! ── What these tests prove ───────────────────────────────────────────────────
//!   1. `stale_position_nft_takeover_is_blocked` — the exploit sequence (attacker
//!      mints on slot 0, closes, victim reuses + funds slot 0) now has its
//!      `TransferPositionOwnership` REJECTED, and the victim keeps full control.
//!   2. `legitimate_same_materialization_transfer_still_works` — a transfer whose
//!      NFT generation matches the current occupant succeeds and moves control
//!      (no regression for the intended feature).
//!   3. `control_transfer_rejected_when_nft_owner_is_not_caller` — the pre-existing
//!      owner gate still holds.

mod common;
#[allow(unused_imports)]
use common::*;

use solana_program::program_option::COption;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    sysvar,
    transaction::Transaction,
};
use spl_token::state::Mint as SplMint;

use percolator_prog::position_nft::{
    self, derive_position_nft, derive_position_nft_mint, PositionNftState, POSITION_NFT_MAGIC,
    POSITION_NFT_STATE_LEN,
};

/// gen-table for slot `idx` lives at `GEN_TABLE_OFF + idx*8`, where
/// `GEN_TABLE_OFF = SLAB_LEN - GEN_TABLE_LEN` and `GEN_TABLE_LEN = MAX_ACCOUNTS*8`
/// (src/percolator.rs constants). Both `SLAB_LEN` and `MAX_ACCOUNTS` come from the
/// harness (`common`), so this stays correct for whatever engine the deployed
/// `.so` was built against.
const GEN_TABLE_OFF: usize = SLAB_LEN - MAX_ACCOUNTS * 8;

/// Read the live per-materialization generation the program stamped for `idx`.
fn read_gen(env: &TestEnv, idx: u16) -> u64 {
    let d = env.svm.get_account(&env.slab).unwrap().data;
    let off = GEN_TABLE_OFF + (idx as usize) * 8;
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

// ── inline helpers (the harness has no NFT/Token-2022 builders) ───────────────

fn token22() -> Pubkey {
    spl_token_2022::id()
}

fn make_nft_mint_data() -> Vec<u8> {
    let mut data = vec![0u8; SplMint::LEN];
    let mut m = SplMint::default();
    m.mint_authority = COption::None;
    m.supply = 1;
    m.decimals = 0;
    m.is_initialized = true;
    m.freeze_authority = COption::None;
    SplMint::pack(m, &mut data).unwrap();
    data
}

fn set_account(env: &mut TestEnv, key: Pubkey, data: Vec<u8>, owner: Pubkey) {
    env.svm
        .set_account(
            key,
            Account {
                lamports: 10_000_000,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

/// Materialize a position-NFT on `slot` stamped with `generation` — the exact
/// bytes `MintPositionNft` writes (`position_nft::write_position_nft_state`,
/// including the new `generation` field) plus a real Token-2022 mint + the owner's
/// Token-2022 ATA holding the single NFT unit. Returns (nft_pda, nft_mint, src_ata).
fn mint_position_nft_for(
    env: &mut TestEnv,
    slot: u16,
    owner: &Pubkey,
    generation: u64,
) -> (Pubkey, Pubkey, Pubkey) {
    let (nft_pda, nft_bump) = derive_position_nft(&env.program_id, &env.slab, slot);
    let (nft_mint, mint_bump) = derive_position_nft_mint(&env.program_id, &env.slab, slot);

    let mut st = PositionNftState {
        magic: POSITION_NFT_MAGIC,
        mint: nft_mint.to_bytes(),
        slab: env.slab.to_bytes(),
        owner: owner.to_bytes(),
        user_idx: slot,
        pending_settlement: 0,
        bump: nft_bump,
        mint_bump,
        generation: [0u8; 8],
        _reserved: [0u8; 11],
    };
    st.set_generation(generation);
    let mut nft_data = vec![0u8; POSITION_NFT_STATE_LEN];
    position_nft::write_position_nft_state(&mut nft_data, &st);
    set_account(env, nft_pda, nft_data, env.program_id);
    set_account(env, nft_mint, make_nft_mint_data(), token22());

    let src_ata = Pubkey::new_unique();
    set_account(
        env,
        src_ata,
        make_token_account_data(&nft_mint, owner, 1),
        token22(),
    );
    (nft_pda, nft_mint, src_ata)
}

/// CALL the real `TransferPositionOwnership` (tag 65). 8 accounts.
fn transfer_position_ownership(
    env: &mut TestEnv,
    current_owner: &Keypair,
    user_idx: u16,
    nft_pda: Pubkey,
    nft_mint: Pubkey,
    src_ata: Pubkey,
    dst_ata: Pubkey,
    new_owner: Pubkey,
) -> Result<(), String> {
    let mut data = vec![65u8];
    data.extend_from_slice(&user_idx.to_le_bytes());
    let ix = Instruction {
        program_id: env.program_id,
        accounts: vec![
            AccountMeta::new(current_owner.pubkey(), true),
            AccountMeta::new(env.slab, false),
            AccountMeta::new(nft_pda, false),
            AccountMeta::new(nft_mint, false),
            AccountMeta::new(src_ata, false),
            AccountMeta::new(dst_ata, false),
            AccountMeta::new_readonly(new_owner, false),
            AccountMeta::new_readonly(token22(), false),
        ],
        data,
    };
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix(), ix],
        Some(&current_owner.pubkey()),
        &[current_owner],
        env.svm.latest_blockhash(),
    );
    env.svm
        .send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e))
}

/// WithdrawCollateral (owner-gated) to a caller-chosen destination ATA.
fn try_withdraw_to(
    env: &mut TestEnv,
    owner: &Keypair,
    user_idx: u16,
    amount: u64,
    dest_ata: Pubkey,
) -> Result<(), String> {
    let (vault_pda, _) =
        Pubkey::find_program_address(&[b"vault", env.slab.as_ref()], &env.program_id);
    let ix = Instruction {
        program_id: env.program_id,
        accounts: vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new(env.slab, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(dest_ata, false),
            AccountMeta::new_readonly(vault_pda, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
            AccountMeta::new_readonly(env.pyth_index, false),
        ],
        data: encode_withdraw(user_idx, amount),
    };
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix(), ix],
        Some(&owner.pubkey()),
        &[owner],
        env.svm.latest_blockhash(),
    );
    env.svm
        .send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e))
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. FIX — the stale-NFT takeover is now BLOCKED, and the victim keeps control.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn stale_position_nft_takeover_is_blocked() {
    program_path();
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);

    let attacker = Keypair::new();
    let beneficiary = Keypair::new();
    let victim = Keypair::new();

    // 1. Attacker takes engine slot 0 and mints a position-NFT bound to THEIR
    //    materialization generation (exactly what MintPositionNft now stamps).
    let a_idx = env.init_user(&attacker);
    assert_eq!(a_idx, 0);
    let attacker_gen = read_gen(&env, 0);
    assert_ne!(attacker_gen, 0, "InitUser stamps a nonzero generation");
    let (nft_pda, nft_mint, src_ata) =
        mint_position_nft_for(&mut env, 0, &attacker.pubkey(), attacker_gen);

    // 2. Attacker closes slot 0 (never burns the NFT — it survives).
    env.close_account(&attacker, 0);

    // 3. Victim reuses slot 0 (LIFO free_head) and funds it. The reused slot gets
    //    a DIFFERENT (higher) materialization generation.
    let _ = env.init_user(&victim);
    env.deposit(&victim, 0, 1_000_000);
    let victim_gen = read_gen(&env, 0);
    assert_ne!(
        attacker_gen, victim_gen,
        "slot reuse must yield a different generation"
    );

    // 4. ATTACK is now REJECTED: the stale NFT's generation != the live generation.
    let dst_ata = Pubkey::new_unique();
    set_account(
        &mut env,
        dst_ata,
        make_token_account_data(&nft_mint, &beneficiary.pubkey(), 0),
        token22(),
    );
    let r = transfer_position_ownership(
        &mut env,
        &attacker,
        0,
        nft_pda,
        nft_mint,
        src_ata,
        dst_ata,
        beneficiary.pubkey(),
    );
    assert!(
        r.is_err(),
        "FIX: stale-NFT TransferPositionOwnership must be rejected, got {r:?}"
    );

    // 5. Victim retains FULL control — owner-gated withdraw still works.
    let vdest = env.create_ata(&victim.pubkey(), 0);
    assert!(
        try_withdraw_to(&mut env, &victim, 0, 100, vdest).is_ok(),
        "FIX: victim keeps ownership of the reused slot"
    );
    // ...and the attacker's beneficiary CANNOT withdraw (never gained ownership).
    env.svm.airdrop(&beneficiary.pubkey(), 1_000_000_000).unwrap();
    let bdest = env.create_ata(&beneficiary.pubkey(), 0);
    assert!(
        try_withdraw_to(&mut env, &beneficiary, 0, 100, bdest).is_err(),
        "FIX: attacker's key never obtained ownership"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. NO REGRESSION — a legitimate transfer (NFT generation == live) still works.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn legitimate_same_materialization_transfer_still_works() {
    program_path();
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);

    let alice = Keypair::new();
    let bob = Keypair::new();

    // Alice owns slot 0 and holds a correctly-bound NFT (generation == live).
    let idx = env.init_user(&alice);
    assert_eq!(idx, 0);
    env.deposit(&alice, 0, 1_000_000);
    let live_gen = read_gen(&env, 0);
    let (nft_pda, nft_mint, src_ata) =
        mint_position_nft_for(&mut env, 0, &alice.pubkey(), live_gen);

    // Alice legitimately transfers position ownership to Bob.
    let dst_ata = Pubkey::new_unique();
    set_account(
        &mut env,
        dst_ata,
        make_token_account_data(&nft_mint, &bob.pubkey(), 0),
        token22(),
    );
    let r = transfer_position_ownership(
        &mut env, &alice, 0, nft_pda, nft_mint, src_ata, dst_ata, bob.pubkey(),
    );
    assert!(
        r.is_ok(),
        "NO REGRESSION: a same-materialization transfer must succeed, got {r:?}"
    );

    // Ownership moved: Bob can withdraw, Alice cannot.
    env.svm.airdrop(&bob.pubkey(), 1_000_000_000).unwrap();
    let bdest = env.create_ata(&bob.pubkey(), 0);
    assert!(
        try_withdraw_to(&mut env, &bob, 0, 100, bdest).is_ok(),
        "NO REGRESSION: new owner gains control"
    );
    let adest = env.create_ata(&alice.pubkey(), 0);
    assert!(
        try_withdraw_to(&mut env, &alice, 0, 100, adest).is_err(),
        "NO REGRESSION: prior owner relinquished control"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. CONTROL — the pre-existing owner gate still holds.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn control_transfer_rejected_when_nft_owner_is_not_caller() {
    program_path();
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);

    let attacker = Keypair::new();
    let beneficiary = Keypair::new();
    let stranger = Keypair::new();

    let idx = env.init_user(&attacker);
    assert_eq!(idx, 0);
    env.deposit(&attacker, 0, 1_000_000);
    let live_gen = read_gen(&env, 0);
    // NFT owned by a stranger (not the caller), even with a matching generation.
    let (nft_pda, nft_mint, src_ata) =
        mint_position_nft_for(&mut env, 0, &stranger.pubkey(), live_gen);

    let dst_ata = Pubkey::new_unique();
    set_account(
        &mut env,
        dst_ata,
        make_token_account_data(&nft_mint, &beneficiary.pubkey(), 0),
        token22(),
    );
    assert!(
        transfer_position_ownership(
            &mut env,
            &attacker,
            0,
            nft_pda,
            nft_mint,
            src_ata,
            dst_ata,
            beneficiary.pubkey(),
        )
        .is_err(),
        "CONTROL: transfer rejected when caller is not the NFT's owner"
    );
}
