mod common;
use common::*;
use solana_sdk::signature::{Keypair, Signer};

#[test]
fn probe_last_market_slot() {
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);
    let lp = Keypair::new();
    let lp_idx = env.init_lp(&lp);
    env.deposit(&lp, lp_idx, 5_000_000_000);
    let user = Keypair::new();
    let user_idx = env.init_user(&user);
    env.deposit(&user, user_idx, 5_000_000_000);
    env.trade(&user, &lp, lp_idx, user_idx, 100_000);

    // Bump slot via set_slot_and_price walk + crank
    env.set_slot_and_price(50, 138_000_000);

    let s = env.svm.get_account(&env.slab).unwrap();
    // last_market_slot should be ~150 (50 + 100 effective offset)
    let needles = [50u64, 51, 100, 150];
    for n in needles {
        let needle = n.to_le_bytes();
        let mut hits = vec![];
        for i in 600..1300 {
            if s.data[i..i + 8] == needle {
                hits.push(i);
            }
        }
        println!("u64={} hits in engine range: {:?}, rel: {:?}",
            n, hits, hits.iter().map(|h| h - 600).collect::<Vec<_>>());
    }
}

/// Probe to find num_used_accounts offset.
/// Creates 3 accounts → num_used should be 3 (u16).
/// Searches the slab for u16=3 pattern to find the offset.
#[test]
fn probe_num_used_accounts_offset() {
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);
    let lp = Keypair::new();
    let lp_idx = env.init_lp(&lp);
    env.deposit(&lp, lp_idx, 5_000_000_000);
    let user1 = Keypair::new();
    let user1_idx = env.init_user(&user1);
    env.deposit(&user1, user1_idx, 5_000_000_000);
    let user2 = Keypair::new();
    let user2_idx = env.init_user(&user2);
    env.deposit(&user2, user2_idx, 5_000_000_000);
    // 3 accounts: LP (0), user1 (1), user2 (2) → num_used = 3

    let s = env.svm.get_account(&env.slab).unwrap();
    // Search for u16 = 3 in the engine area (slab[600..slab.len()-2])
    let needle: [u8; 2] = 3u16.to_le_bytes();
    let mut hits = vec![];
    for i in 600..(s.data.len().saturating_sub(2)) {
        if s.data[i..i + 2] == needle {
            hits.push(i);
        }
    }
    println!("u16=3 hits in slab (absolute): {:?}", &hits[..hits.len().min(20)]);
    println!("u16=3 hits relative to engine+600: {:?}",
        hits.iter().filter(|&&h| h >= 600).map(|h| h - 600).collect::<Vec<_>>().iter().take(20).collect::<Vec<_>>());

    // Also search for bitmap: 3 accounts → bitmap word 0 = 7 (bits 0,1,2 set)
    let bm_needle: [u8; 8] = 7u64.to_le_bytes();
    let mut bm_hits = vec![];
    for i in 600..(s.data.len().saturating_sub(8)) {
        if s.data[i..i + 8] == bm_needle {
            bm_hits.push(i);
        }
    }
    println!("bitmap (u64=7) hits: {:?}, rel: {:?}",
        &bm_hits[..bm_hits.len().min(5)],
        bm_hits.iter().map(|h| h - 600).collect::<Vec<_>>());
}

/// Probe accounts array offset for DEFAULT build.
/// LP deposits a large unique amount + user deposits different unique amount.
/// capital stored = DEFAULT_INIT_CAPITAL(99) + deposit. We search for both.
/// Also probes vault offset by searching for total deposits (vault = sum).
#[test]
fn probe_accounts_array_offset() {
    let mut env = TestEnv::new();
    env.init_market_with_invert(0);

    // LP: init (99) + deposit(5_000_000_000) = capital 5_000_000_099
    let lp = Keypair::new();
    let lp_idx = env.init_lp(&lp);
    env.deposit(&lp, lp_idx, 5_000_000_000);

    let s = env.svm.get_account(&env.slab).unwrap();
    let slab_len = s.data.len();
    println!("slab total length: {}", slab_len);

    // Vault = total deposits including DEFAULT_INIT_PAYMENT(100) for LP init
    // vault = LP_init(100) + LP_deposit(5B) = 5_000_000_100
    // Stored in RiskEngine.vault as U128 = [lo: u64, hi: u64]
    let vault_lo: u64 = 5_000_000_100;
    let vault_needle: [u8; 8] = vault_lo.to_le_bytes();
    let mut vault_hits = vec![];
    for i in 0..(slab_len.saturating_sub(16)) {
        if s.data[i..i + 8] == vault_needle && s.data[i + 8..i + 16] == [0u8; 8] {
            vault_hits.push(i);
        }
    }
    println!("vault={} (u128 lo64 + 8 zero bytes) hits: {:?}, rel600: {:?}",
        vault_lo, vault_hits, vault_hits.iter().map(|h| *h as i64 - 600).collect::<Vec<_>>());

    // LP account capital = 99 + 5_000_000_000 = 5_000_000_099
    let lp_cap: u64 = 5_000_000_099;
    let lp_needle: [u8; 8] = lp_cap.to_le_bytes();
    let mut lp_hits = vec![];
    for i in 600..(slab_len.saturating_sub(16)) {
        if s.data[i..i + 8] == lp_needle && s.data[i + 8..i + 16] == [0u8; 8] {
            lp_hits.push(i);
        }
    }
    println!("lp_cap={} hits: {:?}, rel600: {:?}",
        lp_cap, lp_hits, lp_hits.iter().map(|h| h - 600).collect::<Vec<_>>());

    // Now add a user with different amount: 3_000_000_000 + 99 = 3_000_000_099
    let user = Keypair::new();
    let user_idx = env.init_user(&user);
    env.deposit(&user, user_idx, 3_000_000_000);
    let s2 = env.svm.get_account(&env.slab).unwrap();
    let user_cap: u64 = 3_000_000_099;
    let user_needle: [u8; 8] = user_cap.to_le_bytes();
    let mut user_hits = vec![];
    for i in 600..(s2.data.len().saturating_sub(16)) {
        if s2.data[i..i + 8] == user_needle && s2.data[i + 8..i + 16] == [0u8; 8] {
            user_hits.push(i);
        }
    }
    println!("user_cap={} hits: {:?}, rel600: {:?}",
        user_cap, user_hits, user_hits.iter().map(|h| h - 600).collect::<Vec<_>>());

    if lp_hits.len() == 1 && user_hits.len() == 1 {
        let lp_off = lp_hits[0];
        let user_off = user_hits[0];
        let stride = user_off.saturating_sub(lp_off);
        println!("=== RESULT: ACCOUNTS_OFFSET(engine-rel)={} ACCOUNT_SIZE={} ===",
            lp_off - 600, stride);
    } else {
        println!("=== Could not uniquely identify account offsets ===");
        println!("lp_hits={:?} user_hits={:?}", lp_hits, user_hits);
    }
}

/// Print the actual ENGINE_OFF as computed by the test binary.
#[test]
fn probe_engine_off_constant() {
    use percolator_prog::constants::{ENGINE_OFF, HEADER_LEN, CONFIG_LEN, ENGINE_ALIGN, ENGINE_LEN};
    println!("HEADER_LEN={} CONFIG_LEN={} ENGINE_ALIGN={} ENGINE_OFF={} ENGINE_LEN={}",
        HEADER_LEN, CONFIG_LEN, ENGINE_ALIGN, ENGINE_OFF, ENGINE_LEN);
}
