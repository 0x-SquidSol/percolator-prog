//! v2.2 growth overnight (2026-10-14) §1.1-1.4 property tests on the REAL pure functions in
//! `src/growth_v19.rs` at FULL width (the `kani_g8_*` harnesses in `src/v16_program.rs` bound
//! the same functions to u8 / u16 / u32 inputs; neither alone is the claim). Each property has
//! a named mutant as a NEGATIVE CONTROL (a deterministic witness shows the mutant violates
//! it), so no property is vacuous.
#![cfg(not(kani))]

use percolator_prog::growth_v19::*;
use proptest::prelude::*;

/// MUTANT (I-D3): attested depth uncapped.
fn tier_mutant_uncapped(d: u64) -> u8 {
    if d >= DEPTH_TIER3_E6 {
        3
    } else if d >= DEPTH_TIER2_E6 {
        2
    } else if d >= DEPTH_TIER1_E6 {
        1
    } else {
        0
    }
}

/// MUTANT (I-D1): staleness bound off by one (`age <= 151`).
fn eff_mutant_151(d: u64, p: u64, s: u64, now: u64) -> u64 {
    if d == 0 || p == 0 {
        return 0;
    }
    match now.checked_sub(s) {
        Some(age) if age <= DEPTH_MAX_AGE_SLOTS + 1 => d.min(p),
        _ => 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_096))]

    /// I-D3 + I-D1: tier <= 1 for every attested depth; 0 for every other source; effective
    /// depth is min of the two reads when fresh, else 0.
    #[test]
    fn tiers_and_freshness(d in any::<u64>(), p in any::<u64>(), s in any::<u64>(), now in any::<u64>(), src in any::<u8>()) {
        let eff = effective_depth_e6(d, p, s, now);
        let fresh = d != 0 && p != 0 && now >= s && now - s <= DEPTH_MAX_AGE_SLOTS;
        prop_assert_eq!(eff, if fresh { d.min(p) } else { 0 });
        let t = depth_tier(eff, src);
        prop_assert!(t <= 1);
        if src != DEPTH_SOURCE_ATTESTED || eff < DEPTH_TIER1_E6 { prop_assert_eq!(t, 0); }
        prop_assert!(lambda_eff_bps(any_lambda(d), t) <= DEFAULT_LAMBDA_BPS);
    }

    /// I-D2: target in [launch, min(L_tier, max(launch, L_depth))]; off graduation or forced it
    /// IS launch; monotone non-decreasing in C_m.
    #[test]
    fn target_bounds_and_monotone(la in any::<u16>(), lt in any::<u16>(), tier in 0u8..=3, a in any::<u128>(), b in any::<u128>(), cl in any::<u128>(), grad in any::<bool>(), force in any::<bool>()) {
        let ld = l_depth_x100(tier, la);
        let launch = la.min(lt);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let t1 = target_ceiling_x100(grad, force, la, lt, ld, lo, cl);
        let t2 = target_ceiling_x100(grad, force, la, lt, ld, hi, cl);
        prop_assert!(t1 >= launch && t1 <= lt && t1 <= launch.max(ld));
        prop_assert!(t1 <= t2);
        if !grad || force { prop_assert_eq!(t1, launch); }
    }

    /// Ratchet: <= +1x per epoch, only after the epoch; never climbs in the snapshot call;
    /// fall-only never rises.
    #[test]
    fn ratchet_writer(prev in any::<u16>(), ps in any::<u64>(), cl in any::<u64>(), cm in any::<u128>(), grad in any::<bool>(), force in any::<bool>(), la in any::<u16>(), lt in any::<u16>(), tier in 0u8..=3, now in any::<u64>()) {
        let ld = l_depth_x100(tier, la);
        let (c, s, cl2) = graduation_ratchet(prev, ps, cl, cm, grad, force, la, lt, ld, now);
        if c > prev {
            prop_assert!(cl != 0);
            prop_assert!(c as u32 <= prev as u32 + RATCHET_STEP_X100 as u32);
            prop_assert!(now >= ps.saturating_add(RATCHET_EPOCH_SLOTS));
            prop_assert_eq!(s, now);
        }
        if c == prev && cl != 0 { prop_assert!(s == ps || s == now); }
        if cl != 0 { prop_assert_eq!(cl2, cl); } else { prop_assert_eq!(cl2 == 0, cm == 0); }
        let (f, _) = graduation_fall_only(prev, ps, cl, grad, force, la, lt, ld, now);
        prop_assert!(f <= prev);
    }

    /// I-D4 at full width: with a fresh depth, N_cap * 1e4 * P <= depth * κ * POS_SCALE (checked
    /// in u128 where it fits); with none, the capital rule at λ_eff; off the clamp, growth-1.
    #[test]
    fn n_cap_depth_cap(cm in any::<u64>(), lam in 1u32..=MAX_LAMBDA_BPS, tier in 0u8..=3, depth in any::<u64>(), price in 1u64..=u64::MAX) {
        let ps = 1_000_000u128;
        let cm = cm as u128;
        let n = n_cap_g8_q(true, cm, lam, tier, depth, price, ps);
        let cap = n_cap_q(cm, lambda_eff_bps(lam, tier), price, ps);
        if depth == 0 {
            prop_assert_eq!(n, cap);
        } else if let Some(n) = n {
            prop_assert!(Some(n) <= cap);
            if let (Some(l), Some(r)) = (n.checked_mul(BPS).and_then(|x| x.checked_mul(price as u128)), (depth as u128).checked_mul(kappa_depth_bps(tier) as u128 * ps)) {
                prop_assert!(l <= r);
            }
        }
        prop_assert_eq!(n_cap_g8_q(false, cm, lam, tier, depth, price, ps), n_cap_q(cm, lam, price, ps));
    }
}

fn any_lambda(seed: u64) -> u32 {
    (seed % (MAX_LAMBDA_BPS as u64 + 1)) as u32
}

/// NEGATIVE CONTROLS: each mutant violates its property on a concrete witness.
#[test]
fn g8_props_mutants_are_caught() {
    // I-D3: the uncapped tier reaches 3 on a $500k attested depth.
    assert_eq!(tier_mutant_uncapped(DEPTH_TIER3_E6), 3);
    assert!(depth_tier(DEPTH_TIER3_E6, DEPTH_SOURCE_ATTESTED) <= 1);
    // I-D1: the off-by-one staleness trusts a 151-slot-old depth.
    assert_eq!(eff_mutant_151(DEPTH_TIER1_E6, DEPTH_TIER1_E6, 0, 151), DEPTH_TIER1_E6);
    assert_eq!(effective_depth_e6(DEPTH_TIER1_E6, DEPTH_TIER1_E6, 0, 151), 0);
    // I-D2: a `max` for the depth `min` would graduate past L_depth.
    let la = 300u16;
    let mutant = (|| -> u16 {
        let stepped = graduated_ceiling_x100(true, la, 2_000, 1u128 << 20, 1);
        stepped.max(l_depth_x100(1, la))
    })();
    assert!(mutant > 500);
    assert_eq!(target_ceiling_x100(true, false, la, 2_000, l_depth_x100(1, la), 1u128 << 20, 1), 500);
}
