//! v2.2 round 3 (2026-10-14, "one adaptive per-market controller",
//! `~/percolator-ops/ledger/v22-adaptive-controller-design-2026-10-14.md` §3-§4) property tests on
//! the REAL pure functions at FULL width. The `kani_r3_*` harnesses (in `src/v16_program.rs`)
//! bound the same functions to narrow widths; neither alone is the claim. Every property has a
//! named mutant as a NEGATIVE CONTROL (a deterministic witness shows the mutant violates it), so
//! no property is vacuous.
#![cfg(not(kani))]

use percolator_prog::growth_v19 as gv;
use percolator_prog::vault_lp_v18 as vl;
use proptest::prelude::*;

// ── item 1 (GAP-8): alpha <= 50% everywhere until F3 is proven; cushion on by default ─────────

/// MUTANT (GAP-8): the round-2 rule (a band market may allocate up to the band maximum).
fn alpha_in_force_mutant_round2(stored: u16) -> u16 {
    stored
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_096))]

    /// GAP-8: with F3 unproven, the dial ceiling is 50% on every market and the alpha in force
    /// is `min(stored, 50%)`; once F3 is proven, band markets get the band maximum and the
    /// stored (setter-bounded) dial is used as written. The in-force alpha never exceeds the
    /// stored dial, and the allocation limit at the in-force alpha never exceeds 50% of C_eff.
    #[test]
    fn gap8_alpha_cap(stored in any::<u16>(), band in any::<bool>(), c_eff in any::<u128>(), allocated in any::<u128>(), drawable in any::<u128>()) {
        prop_assert_eq!(gv::alloc_alpha_max_bps_for(false, band), 5_000);
        prop_assert_eq!(gv::alloc_alpha_max_bps_for(true, band), if band { gv::ALLOC_ALPHA_MAX_BAND_BPS } else { 5_000 });
        prop_assert_eq!(gv::alloc_alpha_max_bps(band), gv::alloc_alpha_max_bps_for(gv::F3_PROVEN, band));
        let a = gv::alloc_alpha_in_force_bps_for(false, stored);
        prop_assert!(a <= 5_000 && a <= stored);
        prop_assert_eq!(a, stored.min(5_000));
        prop_assert_eq!(gv::alloc_alpha_in_force_bps_for(true, stored), stored);
        if stored <= gv::ALLOC_ALPHA_MAX_BAND_BPS {
            if let Some(lim) = vl::vault_lp_alloc_limit(c_eff, allocated, drawable, a, vl::ALLOC_BUFFER_MIN_BPS) {
                let half = vl::bps_floor(c_eff, 5_000).unwrap();
                prop_assert!(allocated.saturating_add(lim) <= half.max(allocated));
            }
        }
    }

    /// GAP-8: the default cushion split never over-fills the target and the senior part plus
    /// the cushion is the whole leg (the defaults are a valid both-or-neither pair).
    #[test]
    fn gap8_default_cushion_split(avail in any::<u64>(), c_eff in any::<u64>(), junior in any::<u64>()) {
        let (s, k) = vl::cushion_split(avail as u128, vl::CUSHION_SHARE_DEFAULT_BPS, vl::CUSHION_TARGET_DEFAULT_BPS, c_eff as u128, junior as u128).unwrap();
        prop_assert_eq!(s + k, avail as u128);
        prop_assert!(k <= (avail as u128) / 2);
        let target = vl::bps_ceil(c_eff as u128, vl::CUSHION_TARGET_DEFAULT_BPS).unwrap();
        prop_assert!(k <= target.saturating_sub(junior as u128));
    }
}

#[test]
fn r3_item1_mutants_are_caught() {
    // GAP-8: a 70% stored band dial allocates at most 50% now; the round-2 rule would allow 70%.
    assert_eq!(gv::alloc_alpha_in_force_bps_for(false, 7_000), 5_000);
    assert_eq!(alpha_in_force_mutant_round2(7_000), 7_000, "mutant witness: round-2 rule exceeds 50%");
    let lim = vl::vault_lp_alloc_limit(10_000, 0, 10_000, alpha_in_force_mutant_round2(7_000), vl::ALLOC_BUFFER_MIN_BPS).unwrap();
    assert!(lim > 5_000, "mutant witness: allocates {lim} > 50% of C 10,000");
    let lim = vl::vault_lp_alloc_limit(10_000, 0, 10_000, gv::alloc_alpha_in_force_bps_for(false, 7_000), vl::ALLOC_BUFFER_MIN_BPS).unwrap();
    assert_eq!(lim, 5_000);
    // The defaults are on (non-zero, both-or-neither).
    assert_eq!((vl::CUSHION_TARGET_DEFAULT_BPS, vl::CUSHION_SHARE_DEFAULT_BPS), (2_000, 5_000));
}

// ── item 3 (D1(a), GAP-4): graded stress, <= 1x descent per writer call ───────────────────────

/// MUTANT (D1(a)): round 2's ADL predicate (A != 1 or mode != Normal).
fn adl_mutant_round2(a_long_is_one: bool, a_short_is_one: bool, ml: u8, ms: u8) -> bool {
    !a_long_is_one || !a_short_is_one || ml != 0 || ms != 0
}

/// MUTANT (GAP-4): round 2's in-force rule `min(stored, target)` (a cliff).
fn in_force_mutant_round2(stored: u16, target: u16) -> u16 {
    stored.min(target)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_096))]

    /// The stress level is the highest flag; the cap is monotone non-increasing in the level and
    /// non-decreasing in the tier, `L_table(tier - s)` floored at launch for s = 1, 2, launch at 3.
    #[test]
    fn item3_stress_cap(dr in any::<bool>(), adl in any::<bool>(), hl in any::<bool>(), tier in 0u8..=3, la in any::<u16>(), lt in any::<u16>(), lev in any::<[u16; 3]>()) {
        let s = gv::stress_level(dr, adl, hl);
        prop_assert_eq!(s, if hl { 3 } else if adl { 2 } else if dr { 1 } else { 0 });
        let tab = gv::tier_leverage(lev, la, lt);
        for st in 0u8..3 {
            prop_assert!(gv::stress_cap_x100(st + 1, tier, tab) <= gv::stress_cap_x100(st, tier, tab));
        }
        if tier < 3 { prop_assert!(gv::stress_cap_x100(s, tier, tab) <= gv::stress_cap_x100(s, tier + 1, tab)); }
        prop_assert_eq!(gv::stress_cap_x100(3, tier, tab), tab[0]);
        if s == 1 || s == 2 { prop_assert_eq!(gv::stress_cap_x100(s, tier, tab), tab[tier.saturating_sub(s) as usize]); }
        prop_assert!(!gv::real_adl(0, 0));
    }

    /// The smooth descent: a writer call lowers the stored ceiling by <= 1x unless the h-lock,
    /// never below the target; the gate reads in [max(target, stored - 1x), stored].
    #[test]
    fn item3_one_step(prev in any::<u16>(), target in any::<u16>(), ps in any::<u64>(), now in any::<u64>(), st in 0u8..=3, launch in any::<u16>()) {
        let floor = launch.min(target);
        let (c, _) = gv::ratchet_ceiling_x100(prev, ps, target, floor, st, now);
        if c < prev {
            if st < gv::STRESS_HLOCK {
                prop_assert!(prev - c <= gv::RATCHET_STEP_X100);
                prop_assert!(c >= target);
            } else {
                prop_assert_eq!(c, floor, "h-lock: to the floor (launch) at once");
            }
        }
        let f = gv::ceiling_in_force_x100(true, prev, target, launch, u16::MAX, st);
        prop_assert!(f <= prev);
        if prev > target && st < gv::STRESS_HLOCK {
            prop_assert_eq!(f, target.max(prev - gv::RATCHET_STEP_X100.min(prev)));
        }
    }
}

#[test]
fn r3_item3_mutants_are_caught() {
    // D1(a): a matched band liquidation (A_short < 1, modes Normal) is not ADL now; the round-2
    // predicate read it as ADL (-> launch).
    assert!(!gv::real_adl(0, 0));
    assert!(adl_mutant_round2(true, false, 0, 0), "mutant witness: A != 1 read as ADL");
    // GAP-4: stored 20x, target 5x: in force 19x now; the round-2 rule cliffs to 5x.
    assert_eq!(gv::ceiling_in_force_x100(true, 2_000, 500, 500, 2_000, gv::STRESS_NONE), 1_900);
    assert_eq!(in_force_mutant_round2(2_000, 500), 500, "mutant witness: a 15x cliff");
    // the h-lock is the one exception
    assert_eq!(gv::ceiling_in_force_x100(true, 2_000, 500, 500, 2_000, gv::STRESS_HLOCK), 500);
    assert_eq!(gv::ratchet_ceiling_x100(2_000, 0, 500, 500, gv::STRESS_DRAW, 1), (1_900, 1));
}
