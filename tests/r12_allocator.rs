//! R12 (percolator-security review of S1-2): the permanent portfolio-id allocator must survive
//! RestartAssetOracle on asset 0 (S1-3), otherwise ids are re-issued and a stale NFT can match a
//! later incarnation again. Empirical check at the reviewed commit.
#![cfg(not(kani))]
#![allow(dead_code)]
mod indep_harness;

use indep_harness::*;
use percolator_prog::{ix::Instruction as ProgInstruction, state};
use solana_sdk::{instruction::AccountMeta, signature::{Keypair, Signer}};

fn profile0(env: &V16CuEnv) -> state::AssetOracleProfileV16 {
    state::read_asset_oracle_profile(&env.svm.get_account(&env.market).unwrap().data, 0).unwrap()
}

fn set_asset0_lifecycle_recovery(env: &mut V16CuEnv) {
    let mut m = env.svm.get_account(&env.market).unwrap();
    let profile = state::read_asset_oracle_profile(&m.data, 0).unwrap();
    let (cfg, mut group) = state::read_market(&m.data).unwrap();
    group.assets[0].lifecycle = percolator::AssetLifecycleV16::Recovery;
    state::write_market(&mut m.data, &cfg, &group).unwrap();
    state::write_asset_oracle_profile(&mut m.data, 0, &profile).unwrap();
    env.svm.set_account(env.market, m).unwrap();
}

#[test]
fn r12_restart_asset0_keeps_the_portfolio_id_allocator() {
    let mut env = V16CuEnv::new();
    let a = Keypair::new();
    let b = Keypair::new();
    env.svm.airdrop(&a.pubkey(), 10_000_000_000).unwrap();
    env.svm.airdrop(&b.pubkey(), 10_000_000_000).unwrap();
    let p1 = env.create_portfolio(&a);
    let p2 = env.create_portfolio(&b);
    let (id1, _, _) = env.portfolio_identity(p1);
    let (id2, _, _) = env.portfolio_identity(p2);
    assert_eq!((id1, id2), (1, 2));
    let before = profile0(&env).next_portfolio_id;
    assert_eq!(before, 3);

    set_asset0_lifecycle_recovery(&mut env);
    env.svm.warp_to_slot(5);
    let seq = env.control_sequences(0).oracle_observation + 1;
    let admin = env.admin.insecure_clone();
    let market = env.market;
    env.svm.expire_blockhash();
    let price = state::read_asset_oracle_profile(&env.svm.get_account(&market).unwrap().data, 0)
        .unwrap()
        .oracle_target_price_e6
        .max(1);
    let r = env.send(
        ProgInstruction::RestartAssetOracle {
            asset_index: 0,
            market_id: 1,
            now_slot: 5,
            initial_price: price,
            observation_sequence: seq,
        },
        vec![AccountMeta::new(admin.pubkey(), true), AccountMeta::new(market, false)],
        &[&admin],
    );
    eprintln!("restart result: {r:?}");
    r.expect("restart must succeed for the test to be meaningful (not vacuous)");
    assert_eq!(profile0(&env).next_portfolio_id, before, "allocator preserved by tag 69 on asset 0");

    let c = Keypair::new();
    env.svm.airdrop(&c.pubkey(), 10_000_000_000).unwrap();
    let p3 = env.create_portfolio(&c);
    let (id3, _, _) = env.portfolio_identity(p3);
    assert_eq!(id3, 3, "the next incarnation id continues; ids are never reused");
}
