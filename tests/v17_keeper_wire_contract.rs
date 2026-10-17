// Skip when Kani builds the test suite.
#![cfg(not(kani))]
//! PRE-DEPLOY WIRE CONTRACT GUARD — runs against the COMPILED BPF bytecode.
//!
//! WHY THIS EXISTS: on 2026-08-05 a wrapper built from the wrong checkout was
//! deployed to devnet. That build's PermissionlessCrank decoder still read the
//! pre-W3 53-byte payload (`close_q` + `fee_bps`), while the keeper/SDK 4.3.0
//! send the 29-byte W3 payload. Every crank on every market failed with
//! `InvalidInstructionData`, cranking AND pricing stopped, and the markets were
//! quarantined until the program was rebuilt from the canonical tree.
//!
//! Nothing in the test suite caught it, because every other test constructs the
//! instruction through the wrapper's OWN Rust enum — which is by definition
//! self-consistent. This test instead hand-rolls the EXACT bytes the deployed
//! keeper emits and asserts the compiled program accepts them.
//!
//! Keep byte-for-byte in sync with @percolatorct/sdk `encodePermissionlessCrank`
//! (percolator-sdk `src/abi/instructions.ts`). CURRENT wire (the v18+ observation-hint form;
//! the pre-v18 29-byte `action | asset_index | now_slot | funding | recovery_reason` payload
//! was REMOVED and is now rejected by design, see
//! `compiled_program_rejects_the_removed_29_byte_crank_wire`):
//!   tag u8 | now_slot u64 | n u8 | n x (asset_index u16, oracle_accounts u8)
//!   = 1 + 8 + 1 + 3n bytes
//! The decoder is `v16_program.rs` `5 => { now_slot, n, observations }`; n is bounded by
//! CRANK_OBSERVATION_DECODE_MAX.
use litesvm::LiteSVM;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::Signer;
use std::path::PathBuf;

const IX_TAG_PERMISSIONLESS_CRANK: u8 = 5;

fn program_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/deploy/percolator_prog.so");
    p
}

/// Byte-for-byte mirror of the SDK's `encodePermissionlessCrank`.
fn keeper_encode_permissionless_crank(now_slot: u64, observations: &[(u16, u8)]) -> Vec<u8> {
    let mut d = Vec::with_capacity(10 + 3 * observations.len());
    d.push(IX_TAG_PERMISSIONLESS_CRANK);
    d.extend_from_slice(&now_slot.to_le_bytes());
    d.push(observations.len() as u8);
    for (asset_index, oracle_accounts) in observations {
        d.extend_from_slice(&asset_index.to_le_bytes());
        d.push(*oracle_accounts);
    }
    d
}

/// The legacy 29-byte wire (pre-v18). Kept only for the negative control.
fn legacy_29_byte_crank(now_slot: u64) -> Vec<u8> {
    let mut d = Vec::with_capacity(29);
    d.push(IX_TAG_PERMISSIONLESS_CRANK);
    d.push(0); // action
    d.extend_from_slice(&0u16.to_le_bytes()); // asset_index
    d.extend_from_slice(&now_slot.to_le_bytes());
    d.extend_from_slice(&0i128.to_le_bytes()); // funding_rate_e9
    d.push(0); // recovery_reason
    d
}

fn decode_error_text(data: Vec<u8>) -> String {
    let path = program_path();
    assert!(
        path.exists(),
        "BPF not found at {path:?} -- run `cargo build-sbf --features devnet` first. This test must run \
         against the SAME artifact that will be deployed."
    );
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, &path)
        .expect("compiled wrapper must load into LiteSVM");
    let kp = solana_sdk::signature::Keypair::new();
    let payer = kp.pubkey();
    svm.airdrop(&payer, 10_000_000_000).unwrap();
    // Deliberately junk accounts: we only test that the payload DECODES. A wire mismatch fails
    // at decode with InvalidInstructionData; a correct build gets past decode and fails later
    // on account validation, which is the PASS condition.
    let ix = solana_sdk::instruction::Instruction {
        program_id,
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new(payer, true),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
        ],
        data,
    };
    let tx: solana_sdk::transaction::VersionedTransaction =
        solana_sdk::transaction::Transaction::new_signed_with_payer(
            &[ix],
            Some(&kp.pubkey()),
            &[&kp],
            svm.latest_blockhash(),
        )
        .into();
    match svm.simulate_transaction(tx) {
        Ok(_) => String::new(),
        Err(e) => format!("{e:?}"),
    }
}

#[test]
fn keeper_crank_payload_matches_the_sdk_layout() {
    let d = keeper_encode_permissionless_crank(12_345, &[(0, 0), (3, 2)]);
    assert_eq!(d.len(), 1 + 8 + 1 + 3 * 2, "wire size drifted from tag|now_slot|n|n x (u16,u8)");
    assert_eq!(d[0], IX_TAG_PERMISSIONLESS_CRANK);
    assert_eq!(&d[1..9], &12_345u64.to_le_bytes());
    assert_eq!(d[9], 2);
    assert_eq!(&d[10..13], &[0, 0, 0]);
    assert_eq!(&d[13..16], &[3, 0, 2]);
}

/// THE GUARD: feed the compiled program the keeper's real bytes (empty and with hints) and
/// assert it does NOT reject them as malformed. A build with a different PermissionlessCrank
/// decoder fails here with InvalidInstructionData before touching account state -- the
/// 2026-08-05 outage signature.
#[test]
fn compiled_program_accepts_the_real_keeper_crank_wire() {
    for (label, data) in [
        ("no hints", keeper_encode_permissionless_crank(1, &[])),
        ("one hint", keeper_encode_permissionless_crank(1, &[(0, 0)])),
        ("two hints", keeper_encode_permissionless_crank(1, &[(0, 0), (1, 2)])),
    ] {
        let err_text = decode_error_text(data);
        assert!(
            !err_text.contains("InvalidInstructionData"),
            "THE COMPILED PROGRAM REJECTED THE KEEPER'S CRANK PAYLOAD ({label}) AS MALFORMED.\n\
             This is the 2026-08-05 outage signature: the build expects a different \
             PermissionlessCrank wire format than the deployed keeper emits, so every \
             crank on every market would fail.\n\
             DO NOT DEPLOY THIS ARTIFACT.\nerror: {err_text}"
        );
    }
}

/// Negative control (non-vacuity): the decoder DOES reject malformed payloads with
/// InvalidInstructionData, so the guard above can fail. The removed 29-byte wire is such a
/// payload (it leaves trailing/short bytes against `now_slot | n | hints`).
#[test]
fn compiled_program_rejects_the_removed_29_byte_crank_wire() {
    let err_text = decode_error_text(legacy_29_byte_crank(1));
    assert!(
        err_text.contains("InvalidInstructionData"),
        "the removed 29-byte wire was ACCEPTED -- the guard's detector would be blind: {err_text}"
    );
}
