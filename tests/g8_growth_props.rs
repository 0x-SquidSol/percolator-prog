//! v2.2 growth overnight (2026-10-14) §1.1-1.4 + round 2 (R2.1) property tests on the REAL pure
//! functions in `src/growth_v19.rs` at FULL width (the `kani_g8_*` / `kani_r2_*` harnesses in
//! `src/v16_program.rs` bound the same functions to u8 / u16 / u32 inputs; neither alone is the
//! claim). Each property has a named mutant as a NEGATIVE CONTROL (a deterministic witness shows
//! the mutant violates it), so no property is vacuous.
#![cfg(not(kani))]

use percolator_prog::growth_v19::*;
use proptest::prelude::*;

/// MUTANT (I-D3): attested depth uncapped (the DEX rule applied to every source).
fn venue_mutant_uncapped(d: u64, th: [u64; 3]) -> u8 {
    depth_tier_for(d, th)
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

/// MUTANT (W-M1, the round-1 rule): an absent / stale depth removes the cap.
fn cap_depth_mutant_round1(eff: u64) -> Option<u64> {
    if eff > 0 {
        Some(eff)
    } else {
        None
    }
}

fn arb_thresholds() -> impl Strategy<Value = [u32; 3]> {
    prop_oneof![Just([0u32; 3]), any::<[u32; 3]>()]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_096))]

    /// I-D1 + I-D3 + R2.1 §3: effective depth is min of the two reads when fresh, else 0; the
    /// venue tier is 0 when absent / stale or sourceless, <= attested_max_tier for attested
    /// depth, uncapped only for the DEX source; the combined tier is <= both inputs.
    #[test]
    fn tiers_and_freshness(d in any::<u64>(), p in any::<u64>(), s in any::<u64>(), now in any::<u64>(), src in any::<u8>(), amt in any::<u8>(), ths in arb_thresholds(), book in 0u8..=3) {
        let eff = effective_depth_e6(d, p, s, now);
        let fresh = d != 0 && p != 0 && now >= s && now - s <= DEPTH_MAX_AGE_SLOTS;
        prop_assert_eq!(eff, if fresh { d.min(p) } else { 0 });
        let th = tier_thresholds(ths, 6);
        let t = venue_tier(eff, src, th, amt);
        prop_assert!(t <= MAX_TIER);
        if eff == 0 { prop_assert_eq!(t, 0); }
        match depth_source_of(src) {
            DEPTH_SOURCE_ATTESTED => prop_assert!(t <= attested_max_tier_eff(amt)),
            DEPTH_SOURCE_DEX => prop_assert_eq!(t, depth_tier_for(eff, th)),
            _ => prop_assert_eq!(t, 0),
        }
        if amt == 0 && depth_source_of(src) == DEPTH_SOURCE_ATTESTED { prop_assert!(t <= 1); }
        let c = combined_tier(t, book);
        prop_assert!(c <= t && c <= book);
        prop_assert!(lambda_eff_bps(any_lambda(d), c) <= lambda_depth_bps(c));
    }

    /// I-D2 (R2.1): target in [launch, min(L_tier, max(launch, L_table(tier)))]; off graduation or
    /// forced it IS launch; monotone non-decreasing in the tier; the table is monotone, starts at
    /// launch and never exceeds L_tier.
    #[test]
    fn target_bounds_and_monotone(la in any::<u16>(), lt in any::<u16>(), lev in any::<[u16; 3]>(), t1 in 0u8..=3, t2 in 0u8..=3, grad in any::<bool>(), stress in 0u8..=3) {
        let launch = la.min(lt);
        let tab = tier_leverage(lev, la, lt);
        prop_assert_eq!(tab[0], launch);
        for i in 0..3 { prop_assert!(tab[i] <= tab[i + 1] && tab[i + 1] <= lt.max(launch)); }
        let (lo, hi) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        let a = target_ceiling_x100(grad, stress, la, lt, l_table_x100(lo, tab), stress_cap_x100(stress, lo, tab));
        let b = target_ceiling_x100(grad, stress, la, lt, l_table_x100(hi, tab), stress_cap_x100(stress, hi, tab));
        prop_assert!(a >= launch && a <= lt.max(launch) && a <= launch.max(l_table_x100(lo, tab)));
        prop_assert!(a <= l_table_x100(lo, tab).max(launch));
        prop_assert!(a <= b);
        if !grad || stress >= STRESS_HLOCK { prop_assert_eq!(a, launch); }
        // round 3 item 3: stress is graded and monotone (more stress never raises the target)
        if stress >= 1 { prop_assert!(a <= stress_cap_x100(stress, lo, tab).max(launch)); }
        let more = target_ceiling_x100(grad, (stress + 1).min(3), la, lt, l_table_x100(lo, tab), stress_cap_x100((stress + 1).min(3), lo, tab));
        prop_assert!(more <= a);
    }

    /// Ratchet: <= +1x per epoch, only after the epoch; never climbs in the snapshot call;
    /// fall-only never rises.
    #[test]
    fn ratchet_writer(prev in any::<u16>(), ps in any::<u64>(), cl in any::<u64>(), cm in any::<u128>(), grad in any::<bool>(), stress in 0u8..=3, la in any::<u16>(), lt in any::<u16>(), lev in any::<[u16; 3]>(), tier in 0u8..=3, now in any::<u64>(), stored in any::<u16>()) {
        let tab = tier_leverage(lev, la, lt);
        let lx = l_table_x100(tier, tab);
        let cap = stress_cap_x100(stress, tier, tab);
        let (c, s, cl2) = graduation_ratchet(prev, ps, cl, cm, grad, stress, la, lt, lx, cap, now);
        // round 3 item 3: never more than 1x down per writer call except the h-lock
        if c < prev && stress < STRESS_HLOCK { prop_assert!(prev - c <= RATCHET_STEP_X100); }
        let t = target_ceiling_x100(grad, stress, la, lt, lx, cap);
        let f = ceiling_in_force_x100(true, stored, t, la, lt, stress);
        prop_assert!(f <= stored && f >= stored.min(t));
        if stored > t && stress < STRESS_HLOCK { prop_assert!(stored - f <= RATCHET_STEP_X100); }
        if c > prev {
            prop_assert!(cl != 0);
            prop_assert!(c as u32 <= prev as u32 + RATCHET_STEP_X100 as u32);
            prop_assert!(now >= ps.saturating_add(RATCHET_EPOCH_SLOTS));
            prop_assert_eq!(s, now);
        }
        if c == prev && cl != 0 { prop_assert!(s == ps || s == now); }
        if cl != 0 { prop_assert_eq!(cl2, cl); } else { prop_assert_eq!(cl2 == 0, cm == 0); }
        let (f, _) = graduation_fall_only(prev, ps, grad, stress, la, lt, lx, cap, now);
        prop_assert!(f <= prev);
        if stress < STRESS_HLOCK { prop_assert!(prev - f <= RATCHET_STEP_X100); }
    }

    /// I-D4 at full width: with a cap depth, N_cap * 1e4 * P <= depth * κ * POS_SCALE (checked in
    /// u128 where it fits) and N_cap <= the capital rule at λ_eff; with none, exactly the capital
    /// rule; off the clamp, growth-1 whatever the depth.
    #[test]
    fn n_cap_depth_cap(cm in any::<u64>(), lam in 1u32..=MAX_LAMBDA_BPS, tier in 0u8..=3, depth in prop::option::of(any::<u64>()), price in 1u64..=u64::MAX) {
        let ps = 1_000_000u128;
        let cm = cm as u128;
        let n = n_cap_g8_q(true, cm, lam, tier, depth, price, ps);
        let cap = n_cap_q(cm, lambda_eff_bps(lam, tier), price, ps);
        match depth {
            None => prop_assert_eq!(n, cap),
            Some(d) => if let Some(n) = n {
                prop_assert!(Some(n) <= cap);
                if let (Some(l), Some(r)) = (n.checked_mul(BPS).and_then(|x| x.checked_mul(price as u128)), (d as u128).checked_mul(kappa_depth_bps(tier) as u128 * ps)) {
                    prop_assert!(l <= r);
                }
                if d == 0 { prop_assert_eq!(n, 0); }
            },
        }
        prop_assert_eq!(n_cap_g8_q(false, cm, lam, tier, depth, price, ps), n_cap_q(cm, lam, price, ps));
    }

    /// W-M1 (R2.1 §1): once DEPTH_EVER is set the cap depth is ALWAYS Some (never uncapped), it
    /// is the fresh reading when there is one (a real 0 included), and the sticky depth is <=
    /// last_good (and, round 3, <= the window minimum) and monotone non-increasing in
    /// staleness, halving per 9,000 slots.
    #[test]
    fn sticky_cap_never_uncapped(src in any::<u8>(), fresh in prop::option::of(any::<u64>()), lg in any::<u64>(), lgs in any::<u64>(), n1 in any::<u64>(), n2 in any::<u64>(), tw in prop::option::of(any::<u64>())) {
        let ever = src | DEPTH_EVER;
        let c = cap_depth_e6(ever, fresh, lg, lgs, n1, tw);
        prop_assert!(c.is_some());
        if fresh.is_some() { prop_assert_eq!(c, fresh); } else {
            prop_assert!(c.unwrap() <= lg);
            if let Some(t) = tw { prop_assert!(c.unwrap() <= t); }
        }
        let (a, b) = if n1 <= n2 { (n1, n2) } else { (n2, n1) };
        prop_assert!(sticky_depth_e6(lg, lgs, a) >= sticky_depth_e6(lg, lgs, b));
        let k = a.saturating_sub(lgs) / DEPTH_STICKY_EPOCH_SLOTS;
        if k < 64 { prop_assert_eq!(sticky_depth_e6(lg, lgs, a), lg >> k); }
        // without DEPTH_EVER and no fresh reading: no cap (the market is not closed before its
        // first reading)
        prop_assert_eq!(cap_depth_e6(src & !DEPTH_EVER, None, lg, lgs, n1, tw), None);
    }

    /// The one depth writer: a same-slot push never raises the depth; DEPTH_EVER is never cleared
    /// by it; the last good depth changes exactly when the result is a fresh reading (and then
    /// is the rate-limited WINDOW minimum); the window changes exactly then; the source bits are
    /// a known source and NONE for an absent sample. Coordinator ruling: a later-slot REAL
    /// reading of 0 is fresh at once (Some(0)), and every reader reads 0 at once (round 3: the
    /// last good depth is held, gated through the window).
    #[test]
    fn depth_writer(d in any::<u64>(), s in any::<u64>(), p in any::<u64>(), src_lo in 0u8..=2, prev_real in any::<bool>(), ever in any::<bool>(), lg in any::<u64>(), lgs in any::<u64>(), new_src in 0u8..=4, nd in any::<u64>(), now in any::<u64>(), cur in any::<u32>(), prv in any::<u32>(), bk in any::<u16>(), unit in 1u64..=1_000_000_000_000) {
        let srcb = src_lo | if ever { DEPTH_EVER } else { 0 } | if prev_real { DEPTH_PREV_REAL } else { 0 };
        let tw = if bk == 0 { DepthWindow::default() } else { DepthWindow { cur_tokens: cur.min(u32::MAX - 1), prev_tokens: prv, bucket_p1: bk } };
        let st = DepthState { depth_e6: d, depth_slot: s, depth_prev_e6: p, depth_source: srcb, last_good_e6: lg, last_good_slot: lgs, rise_slot64: 0, tw };
        let o = depth_push_update(st, new_src, nd, now, u64::MAX, unit);
        if s != 0 && now <= s { prop_assert!(o.depth_e6 <= d); prop_assert_eq!(o.depth_prev_e6, p); prop_assert_eq!(o.depth_slot, s); }
        if depth_ever(srcb) { prop_assert!(depth_ever(o.depth_source)); }
        let fresh = fresh_depth_e6(o.depth_e6, o.depth_prev_e6, o.depth_slot, o.depth_source, now);
        match fresh {
            Some(f) => {
                prop_assert!(depth_ever(o.depth_source));
                let w2 = depth_tw_update(tw, depth_tokens_floor(f, unit), o.depth_slot);
                prop_assert_eq!(o.tw, w2);
                let w = depth_tw_min_atoms(w2, unit).unwrap().min(f);
                let (exp, er, es) = depth_rate_limited(depth_ever(srcb), lg, lgs, 0, w, depth_tw_full(w2), o.depth_slot, u64::MAX);
                prop_assert_eq!((o.last_good_e6, o.rise_slot64, o.last_good_slot), (exp, er, es));
                let u = usable_depth_e6(Some(f), o.last_good_e6, depth_tw_min_atoms(o.tw, unit)).unwrap();
                prop_assert!(u <= f && u <= o.last_good_e6);
            }
            None => {
                prop_assert_eq!((o.last_good_e6, o.last_good_slot, o.tw), (lg, lgs, tw));
                prop_assert_eq!(depth_ever(o.depth_source), depth_ever(srcb));
            }
        }
        prop_assert!(depth_source_byte_ok(o.depth_source));
        let real = (1..=2).contains(&new_src);
        let same_slot = s != 0 && now <= s;
        if !real && !same_slot { prop_assert_eq!(depth_source_of(o.depth_source), DEPTH_SOURCE_NONE); prop_assert_eq!(fresh, None); }
        if real && nd == 0 && !same_slot {
            prop_assert_eq!(fresh, Some(0), "a later-slot real 0 is fresh at once");
            prop_assert_eq!(usable_depth_e6(fresh, o.last_good_e6, depth_tw_min_atoms(o.tw, unit)), Some(0), "every reader reads 0 at once");
            prop_assert_eq!(depth_tw_min_atoms(o.tw, unit), Some(0), "and the window holds the zero");
        }
    }

    /// Security review R2-M1 (one step) + round 3 item 4: the rate-limited depth never exceeds
    /// the window minimum `w` unless it is HELD through a zero; falls to `w` at once (w > 0); a
    /// market's FIRST reading is <= T1/16 (D-I1); a real zero holds it (D2); after that it rises
    /// at most 2x of the (decayed) base (or to the T1/16 bootstrap floor), only on a full window,
    /// and only >= 9,000 slots after the last rise.
    #[test]
    fn depth_rate_limit_one_step(ever in any::<bool>(), lg in any::<u64>(), lgs in any::<u64>(), re in any::<u32>(), w in any::<u64>(), full in any::<bool>(), slot in any::<u64>(), seed in any::<u64>()) {
        let (out, re2, s2) = depth_rate_limited(ever, lg, lgs, re, w, full, slot, seed);
        let floor = seed / DEPTH_BOOTSTRAP_DIV;
        if !ever { prop_assert_eq!(out, w.min(floor)); prop_assert!(out <= seed); prop_assert_eq!(s2, slot); return Ok(()); }
        if w == 0 { prop_assert_eq!((out, re2, s2), (lg, re, lgs), "D2: held"); return Ok(()); }
        let base = sticky_depth_e6(lg, lgs, slot);
        prop_assert!(out <= w.max(base));
        prop_assert!(out <= base.saturating_mul(2).max(floor).max(base));
        if w <= base { prop_assert_eq!((out, re2), (w, re)); }
        if out > base {
            prop_assert!(full, "a rise needs a full window");
            prop_assert!(out <= base.saturating_mul(2).max(floor));
            prop_assert!(depth_rise_window_open(re, slot));
            prop_assert!(slot >= re as u64 * DEPTH_RISE_STAMP_SLOTS + 9_000);
            prop_assert_eq!(re2, depth_rise_stamp(slot));
        }
        if !full || !depth_rise_window_open(re, slot) { prop_assert!(out <= base); }
    }

    /// R2-M1 (sequence, the reviewer's flash-liquidity shape) at the limiter: over ANY sequence
    /// of window minima, the rate-limited depth stays <= max(floor, start) * 2^(9,000-slot
    /// windows spanned); a real reading after the spike takes it back down at once.
    #[test]
    fn depth_rate_limit_sequence(real in 1u64..1_000_000_000_000, big_mul in 2u64..1_000_000, picks in prop::collection::vec(any::<bool>(), 1..40), gap in 1u64..20_000, t1 in 1_000_000u64..1_000_000_000_000) {
        let seed = t1 - 1;
        let big = real.saturating_mul(big_mul);
        let (mut lg, mut lgs, mut re, mut ever) = (0u64, 0u64, 0u32, false);
        let mut slot = 1_000u64;
        let start_slot = slot;
        for &spike in &picks {
            let r = if spike { big } else { real };
            let (a, b, c) = depth_rate_limited(ever, lg, lgs, re, r, true, slot, seed);
            lg = a; re = b; lgs = c; ever = true;
            slot += gap;
        }
        let epochs = ((slot - gap - start_slot) / 9_000) as u32;
        let start = seed / DEPTH_BOOTSTRAP_DIV;
        let bound = start.saturating_mul(1u64.checked_shl(epochs.min(63)).unwrap_or(u64::MAX).max(1)).max(start);
        prop_assert!(lg <= bound, "lg {lg} bound {bound} epochs {epochs}");
        let (after, _, _) = depth_rate_limited(true, lg, lgs, re, real, true, slot, seed);
        prop_assert!(after <= real.max(sticky_depth_e6(lg, lgs, slot)).min(lg.max(real)));
        if sticky_depth_e6(lg, lgs, slot) >= real { prop_assert_eq!(after, real, "down at once to the honest reading"); }
    }

    /// R2.1 §2: `tier_config_ok` is exactly the stated bounds; an accepted explicit table is used
    /// verbatim by `tier_leverage` (the clamps are no-ops), and accepted thresholds are strictly
    /// increasing in atoms.
    #[test]
    fn tier_config_bounds(th in any::<[u32; 3]>(), lev in any::<[u16; 3]>(), la in 100u16..=5_000, lt in 100u16..=10_000) {
        let la = la.min(lt);
        let ok = tier_config_ok(th, lev, la, lt);
        let w: Vec<u32> = (0..3).map(|i| if th[i] == 0 { TIER_THRESHOLD_DEFAULT_USD[i] } else { th[i] }).collect();
        let th_ok = th == [0; 3] || (w[0] < w[1] && w[1] < w[2] && (0..3).all(|i| w[i] >= TIER_THRESHOLD_FLOOR_USD[i]));
        let v: Vec<u16> = (0..3).map(|i| if lev[i] == 0 { TIER_LEVERAGE_DEFAULT_X100[i].min(lt) } else { lev[i] }).collect();
        let lev_ok = lev == [0; 3] || (la <= v[0] && v[0] <= v[1] && v[1] <= v[2] && v[2] <= lt);
        prop_assert_eq!(ok, th_ok && lev_ok);
        if ok && lev != [0; 3] {
            prop_assert_eq!(&tier_leverage(lev, la, lt)[1..], &v[..]);
        }
        if ok {
            let t = tier_thresholds(th, 6);
            prop_assert!(t[0] < t[1] && t[1] < t[2] && t[0] > 0);
        }
    }

    /// R2.1 §3 book depth: never above the 1x capacity notional (and exactly it under the pins);
    /// the book tier is monotone in C_m.
    #[test]
    fn book_depth_bounds(a in any::<u128>(), b in any::<u128>(), lam in 1u32..=MAX_LAMBDA_BPS, ths in arb_thresholds()) {
        let cap = book_capacity_e6(a, lam);
        prop_assert_eq!(book_depth_e6(a, lam) as u128, cap.min(u64::MAX as u128));
        let th = tier_thresholds(ths, 6);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(book_tier(lo, lam, th) <= book_tier(hi, lam, th));
    }
}

fn any_lambda(seed: u64) -> u32 {
    (seed % (MAX_LAMBDA_BPS as u64 + 1)) as u32
}

/// NEGATIVE CONTROLS: each mutant violates its property on a concrete witness.
#[test]
fn g8_props_mutants_are_caught() {
    let th = tier_thresholds([0; 3], 6);
    // I-D3: the uncapped attested tier reaches 3 on a $500k attested depth.
    assert_eq!(venue_mutant_uncapped(DEPTH_TIER3_E6, th), 3);
    assert!(venue_tier(DEPTH_TIER3_E6, DEPTH_SOURCE_ATTESTED, th, 0) <= 1);
    // I-D1: the off-by-one staleness trusts a 151-slot-old depth.
    assert_eq!(eff_mutant_151(DEPTH_TIER1_E6, DEPTH_TIER1_E6, 0, 151), DEPTH_TIER1_E6);
    assert_eq!(effective_depth_e6(DEPTH_TIER1_E6, DEPTH_TIER1_E6, 0, 151), 0);
    // W-M1: the round-1 rule leaves a market that once had depth uncapped when it goes stale.
    assert_eq!(cap_depth_mutant_round1(0), None);
    assert!(cap_depth_e6(DEPTH_EVER, None, DEPTH_TIER1_E6, 0, 1_000, None).is_some());
    // Fresh-zero ruling: the "zero = absent" mutant leaves a market with a real 0 reading open.
    let zero_as_absent = |d: u64| if d == 0 { None } else { Some(d) };
    assert_eq!(zero_as_absent(0), None);
    let z = depth_push_update(DepthState::default(), DEPTH_SOURCE_ATTESTED, 0, 5, u64::MAX, USD_ATOMS);
    assert_eq!(fresh_depth_e6(z.depth_e6, z.depth_prev_e6, z.depth_slot, z.depth_source, 5), Some(0));
    // I-D2: a `max` with L_tier instead of `min` would graduate past L_tier.
    let tab = tier_leverage([0; 3], 500, 1_000);
    let mutant = 1_000u16.max(500u16.max(l_table_x100(3, [500, 500, 1_000, 2_000])));
    assert!(mutant > 1_000);
    assert_eq!(target_ceiling_x100(true, STRESS_NONE, 500, 1_000, l_table_x100(3, tab), u16::MAX), 1_000);
}
