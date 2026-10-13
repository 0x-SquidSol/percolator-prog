#![cfg(not(kani))]
//! Catch-up cap validation (2026-10-13): swordcat's REAL on-chain InitMarket bytes (devnet tx
//! 2D7apCc6..., market 6fonYScx...) replayed through the DEPLOYED wrapper BPF in LiteSVM with only
//! `max_price_move_bps_per_slot` / `max_accrual_dt_slots` changed. Run with
//! `GROWTH_WRAPPER_SO=<deployed .so>`; prints one line per (cap, dt) pair.
use litesvm::LiteSVM;
use percolator_prog::{ix::Instruction as ProgInstruction, state};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use spl_token::state::Mint;

const SWORDCAT_INIT_HEX: &str = "000100e803000000000000a08601000000000070dfa9010000000040420f0000000000000000000000000080841e00000000000000000000000000e803000000000000d007000000000000bc020000000000001e00000000000000320000000000000000e40b54020000000000000000000000000000000000000000000000000000000100000000000000f4010000000000006f00000000000000f4010000000000000a000000000000000a00000000000000f4010000000000000010a5d4e80000000000000000000000000000000000000000000000000000000000f401046400000088138200580200002823000000e1f50500000000";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn patched(cap: u64, dt: u64) -> ProgInstruction {
    let mut ix = ProgInstruction::decode(&unhex(SWORDCAT_INIT_HEX)).expect("decode swordcat init");
    let ProgInstruction::InitMarketV22 { market, .. } = &mut ix else { panic!("not InitMarketV22") };
    let ProgInstruction::InitMarket { max_price_move_bps_per_slot, max_accrual_dt_slots, .. } = market.as_mut()
    else { panic!("inner not InitMarket") };
    *max_price_move_bps_per_slot = cap;
    *max_accrual_dt_slots = dt;
    ix
}

fn try_init(cap: u64, dt: u64) -> Result<Vec<u8>, String> {
    let so = std::env::var("GROWTH_WRAPPER_SO").expect("GROWTH_WRAPPER_SO");
    let mut svm = LiteSVM::new();
    let program_id = percolator_prog::id();
    svm.add_program(program_id, &std::fs::read(so).unwrap());
    let admin = Keypair::new();
    svm.airdrop(&admin.pubkey(), 10_000_000_000).unwrap();
    let market = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let mut mint_data = vec![0u8; Mint::LEN];
    Mint::pack(
        Mint { mint_authority: COption::None, supply: 0, decimals: 6, is_initialized: true, freeze_authority: COption::None },
        &mut mint_data,
    )
    .unwrap();
    let acct = |data: Vec<u8>, owner: Pubkey| Account { lamports: 1_000_000_000, data, owner, executable: false, rent_epoch: 0 };
    svm.set_account(mint, acct(mint_data, spl_token::ID)).unwrap();
    let len = state::market_account_len_for_capacity(1).unwrap();
    assert_eq!(len, 4059, "swordcat slab length");
    svm.set_account(market, acct(vec![0u8; len], program_id)).unwrap();
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(admin.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(mint, false),
        ],
        data: patched(cap, dt).encode(),
    };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&admin.pubkey()), &[&admin], svm.latest_blockhash());
    match svm.send_transaction(tx) {
        Ok(_) => Ok(svm.get_account(&market).unwrap().data),
        Err(e) => Err(format!("{:?}", e.err)),
    }
}

#[test]
fn swordcat_bytes_roundtrip_and_baseline() {
    let raw = unhex(SWORDCAT_INIT_HEX);
    let ix = ProgInstruction::decode(&raw).unwrap();
    println!("SWORDCAT_INIT {:?}", ix);
    assert_eq!(ix.encode(), raw, "encode(decode(x)) == x");
    assert_eq!(patched(1, 500).encode(), raw, "patch (1,500) is the identity");
    assert!(try_init(1, 500).is_ok(), "the deployed swordcat parameters must be accepted");
}

#[test]
fn cap4_dt125_accepted_and_controls() {
    let mut lines = Vec::new();
    for (cap, dt) in [
        (1u64, 500u64), (4, 125), (2, 250), (4, 124), (4, 126), (4, 150), (4, 250), (4, 500),
        (5, 100), (5, 125), (6, 83), (7, 71), (8, 62), (8, 125), (8, 500), (8, 1), (10, 50), (3, 166), (2, 500),
    ] {
        let r = try_init(cap, dt);
        lines.push(format!("PAIR cap={cap} dt={dt} budget_bps={} -> {}", cap * dt, match &r { Ok(_) => "ACCEPTED".to_string(), Err(e) => format!("REJECTED {e}") }));
    }
    for l in &lines { println!("{l}"); }
    let d = try_init(4, 125).expect("cap 4 / dt 125 must be accepted");
    let (_cfg, group) = { let mut d2 = d.clone(); let v = state::market_view_mut(&mut d2).unwrap(); (v.0, (v.1.header.config.max_price_move_bps_per_slot.get(), v.1.header.config.max_accrual_dt_slots.get(), v.1.header.config.band_bps.get(), v.1.header.config.band_max_epoch_slots.get())) };
    println!("READBACK cap/dt/band/epoch = {:?}", group);
    assert_eq!((group.0, group.1), (4, 125));
    assert!(try_init(8, 500).is_err(), "negative control: cap 8 at today's margins / dt must be rejected");
    assert!(try_init(8, 125).is_err(), "negative control: cap 8 at dt 125 (1,000 bps budget)");
    assert!(try_init(4, 500).is_err(), "negative control: cap 4 without shrinking dt (2,000 bps budget)");
    // Measured, and NOT what the 2026-10-13 diagnosis predicted: on a BAND market the growth-1
    // `r_gap >= cap * 50` floor does not apply (r_gap is derived, the Band Safety Law replaces it),
    // so cap 8 with a 496 bps budget is accepted.
    assert!(try_init(8, 62).is_ok(), "cap 8 / dt 62 is accepted on a band market");
}
