//! Polymarket oracle ring-buffer helpers.
//!
//! Pure-function fixed-point math for managing the 60-entry
//! `OracleSnapshotEntry` ring stored on `MarketConfig.oracle_ring_buf`
//! for `market_kind = 2` (PerpOnPolymarket) markets.
//!
//! Read by the matcher CPI at trade time (via `ring_buf_twap`);
//! written by `PushOracleSnapshot` (a future wrapper handler) under
//! signer + monotonic-timestamp + deviation guards.
//!
//! # Integer-overflow proof
//!
//! Storage capacity is fixed at 60 entries. Domain bounds on the
//! inputs:
//!
//! - `p_yes_e6` is `u64`, clamped on write to `[POLY_CLAMP_LO,
//!   POLY_CLAMP_HI] = [10_000, 990_000]` e6 (well under `2^20`).
//! - Staleness weights in TWAP are gated by `MAX_STALENESS_SLOTS = 720`
//!   (under `2^10`).
//! - `count` in TWAP is at most 60 (under `2^6`).
//!
//! Derived bounds on the arithmetic:
//!
//! - `sum_p` in `ring_buf_twap` is at most `60 * POLY_CLAMP_HI`
//!   ≈ `60 * 2^20` ≈ `2^26`, fits in `u64` with `~2^38` margin.
//! - `sum_p / count` is bounded above by `POLY_CLAMP_HI` itself
//!   (by the standard average inequality), so the return value
//!   stays in domain.
//! - `now_slot.saturating_sub(entry.on_chain_slot)` cannot underflow
//!   (saturating semantics); the impossible case of `on_chain_slot >
//!   now_slot` (clock skew / replay) is treated as age = 0.
//!
//! Release profile has `overflow-checks = true` (per `Cargo.toml`),
//! so any unexpected overflow would panic rather than silently wrap.
//! On the bounded inputs above, overflow is unreachable.

use crate::state::OracleSnapshotEntry;

/// Lower bound of the engine's bounded probability domain. Probabilities
/// below this (= 1%) cause leverage math to diverge under the asymmetric
/// leverage cap; the wrapper clamps incoming snapshots to this floor.
pub const POLY_CLAMP_LO: u64 = 10_000;

/// Upper bound of the engine's bounded probability domain (= 99%).
/// Mirror of `POLY_CLAMP_LO` for the high side.
pub const POLY_CLAMP_HI: u64 = 990_000;

/// Maximum age (in Solana slots) at which a snapshot still participates
/// in the TWAP. Older snapshots are excluded. 720 slots ≈ 5 minutes at
/// 400 ms / slot, matching the nominal ring window (60 entries at ~12
/// slots per push).
pub const MAX_STALENESS_SLOTS: u64 = 720;

/// Clamp a raw probability reading to the engine's bounded domain.
///
/// Probabilities outside `[POLY_CLAMP_LO, POLY_CLAMP_HI]` are pulled
/// in to the boundary. The wrapper's `PushOracleSnapshot` handler
/// applies this on every write so the ring never carries an
/// out-of-domain value.
#[inline]
pub fn ring_buf_clamp(p_yes_e6: u64) -> u64 {
    p_yes_e6.clamp(POLY_CLAMP_LO, POLY_CLAMP_HI)
}

/// Return a reference to the most-recently-written entry in the ring
/// — the entry with the largest `source_timestamp`. Returns `None` if
/// the ring is empty (every entry has `source_timestamp == 0`, which
/// is the zero-default for a freshly-initialised slab).
///
/// Used by `PushOracleSnapshot` to enforce strict monotonicity on
/// incoming `source_timestamp` values: the new entry's timestamp must
/// be strictly greater than `ring_buf_last(buf).source_timestamp`.
pub fn ring_buf_last(buf: &[OracleSnapshotEntry; 60]) -> Option<&OracleSnapshotEntry> {
    let mut best: Option<&OracleSnapshotEntry> = None;
    for entry in buf.iter() {
        if entry.source_timestamp == 0 {
            continue;
        }
        match best {
            None => best = Some(entry),
            Some(b) if entry.source_timestamp > b.source_timestamp => best = Some(entry),
            _ => {}
        }
    }
    best
}

/// Insert a new entry into the ring, overwriting the slot with the
/// oldest `source_timestamp`. Returns the index that was overwritten.
///
/// Tie-break rule: when multiple slots share the lowest
/// `source_timestamp` (including the bootstrap case where every slot
/// is at the zero-default), the lowest-index slot wins. As a
/// consequence, the first 60 pushes into a fresh ring fill slots
/// `0..60` in order; subsequent pushes rotate against the oldest.
///
/// This function performs NO validation of `entry` — no clamp, no
/// monotonicity check, no deviation guard. Those checks are the
/// caller's responsibility (and live in `PushOracleSnapshot` before
/// it calls into this primitive). `ring_buf_push` is a pure-data
/// rotation helper.
pub fn ring_buf_push(
    buf: &mut [OracleSnapshotEntry; 60],
    entry: OracleSnapshotEntry,
) -> usize {
    let mut oldest_idx: usize = 0;
    let mut oldest_ts: i64 = buf[0].source_timestamp;
    for (i, e) in buf.iter().enumerate().skip(1) {
        if e.source_timestamp < oldest_ts {
            oldest_ts = e.source_timestamp;
            oldest_idx = i;
        }
    }
    buf[oldest_idx] = entry;
    oldest_idx
}

/// Compute the time-weighted average of `p_yes_e6` across the entries
/// that are within `MAX_STALENESS_SLOTS` of `now_slot`. Returns `None`
/// if no entry qualifies (every entry stale or the ring is empty).
///
/// Uses a uniform-weight TWAP within the staleness window: each
/// in-window entry contributes equally to the average. This is
/// intentionally simpler than an EWMA — the bounded-domain leverage
/// math + asymmetric clamp already discourage trading at probability
/// extremes, so the TWAP's job is to defeat single-block manipulation
/// rather than produce a finely-tuned recency-weighted estimate.
///
/// All arithmetic stays inside `u64` per the integer-overflow proof
/// in the module header.
pub fn ring_buf_twap(buf: &[OracleSnapshotEntry; 60], now_slot: u64) -> Option<u64> {
    let mut sum_p: u64 = 0;
    let mut count: u64 = 0;
    for entry in buf.iter() {
        // Skip never-written slots. `source_timestamp == 0` is the
        // zero-default and indicates the slot has never carried a
        // real snapshot (legitimate snapshots always come from a
        // positive Polymarket unix timestamp).
        if entry.source_timestamp == 0 {
            continue;
        }
        // Skip stale entries (age > MAX_STALENESS_SLOTS). Saturating
        // subtraction is defensive against the impossible case of
        // `on_chain_slot > now_slot` (clock skew / replay) — that
        // collapses to age = 0, which is in-window.
        let age = now_slot.saturating_sub(entry.on_chain_slot);
        if age > MAX_STALENESS_SLOTS {
            continue;
        }
        sum_p = sum_p.saturating_add(entry.p_yes_e6);
        count += 1;
    }
    // `checked_div` collapses the "no entries qualify → None" branch with
    // the divide-by-zero guard. `count == 0` returns `None`; any positive
    // count returns `Some(sum_p / count)`. Semantically identical to an
    // explicit `if count == 0 { None } else { Some(sum_p / count) }`.
    sum_p.checked_div(count)
}

/// On-chain price → probability formula for Pyth-typed Polymarket-perp
/// markets. Pure-fn, deterministic, bounded-domain output. The function
/// is the value-anchoring half of `PushOracleSnapshot`: when a keeper
/// submits a `p_yes_e6`, the wrapper computes this independently from
/// the Pyth price and the per-market `(threshold, scale)` configuration,
/// then rejects the submission if `|caller - on_chain|` exceeds the
/// per-market `value_deviation_bps * 100` tolerance.
///
/// Formula:
/// ```text
///   delta_e6   = pyth_price_e6 - threshold_e6                  (signed i128)
///   p_change   = scale_bps_per_pct × delta_e6 × 10_000 / threshold_e6
///   p_yes_e6   = clamp(500_000 + p_change, POLY_CLAMP_LO, POLY_CLAMP_HI)
/// ```
///
/// Algebraic derivation:
///   * `delta_e6 / threshold_e6 × 100` is the percent change in price.
///   * Multiplied by `scale_bps_per_pct` gives the change in `p_yes` in
///     bps of probability.
///   * One bps of probability = `e6 / 10_000 = 100` in e6 units, so the
///     change in e6 is `× 100`.
///   * Combined: `p_change_e6 = scale × delta × 100 × 100 / threshold
///     = scale × delta × 10_000 / threshold`. The order
///     `scale × delta × 10_000` first, then `/ threshold` last, keeps
///     integer-truncation error to at most one e6 unit per division.
///
/// Domain assumptions (caller must enforce; otherwise the function
/// returns a clamped-but-meaningless value):
///   * `threshold_e6 > 0` — the `SetPythPriceMapping` setter rejects
///     zero, so this is held by every configured market.
///   * `scale_bps_per_pct != 0` — likewise rejected by the setter.
///
/// # Integer-overflow analysis
///
/// All intermediate arithmetic uses `i128` (`scale * delta * 10_000`
/// followed by `/ threshold`). Bounds:
///   * `scale_bps_per_pct: i32` → at most `≈ 2^31` in absolute value.
///   * `delta_e6: i128` → bounded above by `pyth_price_e6: u64` (≈ 2^64)
///     when threshold = 0, but `threshold > 0` is required; in any
///     legitimate use `|delta| ≤ pyth_price + threshold < 2^65`.
///   * `10_000 ≈ 2^14`.
///   * Worst-case product: `2^31 × 2^65 × 2^14 = 2^110`.
///   * `i128::MAX ≈ 2^127`, leaving `2^17` headroom — comfortable.
///
/// In `release` profile, `overflow-checks = true`, so any unexpected
/// overflow would panic rather than silently wrap. On the bounded
/// inputs above, overflow is unreachable.
///
/// Edge cases the function handles cleanly:
///   * `pyth_price_e6 == threshold_e6` → `delta = 0` → `p_change = 0`
///     → returns exactly `500_000` (midpoint).
///   * `scale_bps_per_pct < 0` → returns a probability that falls as
///     price rises (supports "X stays below threshold" market phrasing).
///   * Very large `|delta_e6|` → output clamps to `POLY_CLAMP_LO` or
///     `POLY_CLAMP_HI`.
///   * `scale_bps_per_pct == i32::MIN` does NOT trigger any abs/negation
///     bug; the function multiplies through with widened `i128` only.
pub fn pyth_price_to_p_yes_e6(
    pyth_price_e6: u64,
    threshold_e6: u64,
    scale_bps_per_pct: i32,
) -> u64 {
    if threshold_e6 == 0 {
        // Defensive: callers should already reject zero-threshold, but
        // a fallback to midpoint avoids a panicking division. Returning
        // midpoint is the most conservative reading.
        return 500_000;
    }
    let price = pyth_price_e6 as i128;
    let threshold = threshold_e6 as i128;
    let delta = price - threshold;

    // Plain `*` (not `wrapping_mul`) so the release profile's
    // `overflow-checks = true` setting catches any future bounds drift
    // as a panic rather than a silent wrap that would corrupt the
    // deviation guard. The overflow-proof in the module header shows
    // that on the legitimate input domain `(scale ∈ [-2^31, 2^31),
    // |delta| < 2^65, 10_000 < 2^14)` the product stays under `2^110`
    // — well within `i128::MAX ≈ 2^127`.
    let numerator = (scale_bps_per_pct as i128) * delta * 10_000;
    let p_change = numerator / threshold;
    let p_signed = 500_000i128 + p_change;

    p_signed.clamp(POLY_CLAMP_LO as i128, POLY_CLAMP_HI as i128) as u64
}

/// Absolute difference between two e6 probabilities. Symmetric in its
/// arguments. Cheap helper used by `PushOracleSnapshot`'s deviation
/// guard: `value_deviation_e6(caller_p, formula_p) <= deviation_bps × 100`.
#[inline]
pub fn value_deviation_e6(a: u64, b: u64) -> u64 {
    a.abs_diff(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OracleSnapshotEntry;

    fn empty_buf() -> [OracleSnapshotEntry; 60] {
        [OracleSnapshotEntry {
            p_yes_e6: 0,
            source_timestamp: 0,
            on_chain_slot: 0,
        }; 60]
    }

    // ----- ring_buf_clamp -----

    #[test]
    fn clamp_within_domain_passes_through() {
        assert_eq!(ring_buf_clamp(500_000), 500_000);
        assert_eq!(ring_buf_clamp(POLY_CLAMP_LO), POLY_CLAMP_LO);
        assert_eq!(ring_buf_clamp(POLY_CLAMP_HI), POLY_CLAMP_HI);
    }

    #[test]
    fn clamp_below_lo_pulled_up() {
        assert_eq!(ring_buf_clamp(0), POLY_CLAMP_LO);
        assert_eq!(ring_buf_clamp(POLY_CLAMP_LO - 1), POLY_CLAMP_LO);
    }

    #[test]
    fn clamp_above_hi_pulled_down() {
        assert_eq!(ring_buf_clamp(POLY_CLAMP_HI + 1), POLY_CLAMP_HI);
        assert_eq!(ring_buf_clamp(1_000_000), POLY_CLAMP_HI);
        assert_eq!(ring_buf_clamp(u64::MAX), POLY_CLAMP_HI);
    }

    // ----- ring_buf_last -----

    #[test]
    fn empty_ring_has_no_last() {
        assert!(ring_buf_last(&empty_buf()).is_none());
    }

    #[test]
    fn last_returns_max_timestamp_entry() {
        let mut buf = empty_buf();
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 420_000,
                source_timestamp: 100,
                on_chain_slot: 50,
            },
        );
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 510_000,
                source_timestamp: 200,
                on_chain_slot: 150,
            },
        );
        let last = ring_buf_last(&buf).unwrap();
        assert_eq!(last.source_timestamp, 200);
        assert_eq!(last.p_yes_e6, 510_000);
    }

    // ----- ring_buf_push -----

    #[test]
    fn push_into_fresh_ring_fills_slot_zero_first() {
        let mut buf = empty_buf();
        let idx = ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 420_000,
                source_timestamp: 100,
                on_chain_slot: 50,
            },
        );
        assert_eq!(idx, 0);
        assert_eq!(buf[0].p_yes_e6, 420_000);
        // Slots 1..60 untouched
        assert_eq!(buf[1].source_timestamp, 0);
        assert_eq!(buf[59].source_timestamp, 0);
    }

    #[test]
    fn push_fills_in_order_during_bootstrap() {
        let mut buf = empty_buf();
        for i in 0..60u64 {
            let idx = ring_buf_push(
                &mut buf,
                OracleSnapshotEntry {
                    p_yes_e6: 500_000 + i,
                    source_timestamp: (i as i64) + 1,
                    on_chain_slot: i + 1,
                },
            );
            assert_eq!(idx, i as usize, "bootstrap pushes should fill 0..60 in order");
        }
    }

    #[test]
    fn push_rotates_after_filling() {
        let mut buf = empty_buf();
        for i in 0..60u64 {
            ring_buf_push(
                &mut buf,
                OracleSnapshotEntry {
                    p_yes_e6: 500_000 + i,
                    source_timestamp: (i as i64) + 1,
                    on_chain_slot: i + 1,
                },
            );
        }
        // The 61st push overwrites the oldest (slot 0, source_timestamp = 1)
        let idx = ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 600_000,
                source_timestamp: 61,
                on_chain_slot: 100,
            },
        );
        assert_eq!(idx, 0);
        assert_eq!(buf[0].p_yes_e6, 600_000);
        // Slot 1 (the next-oldest, source_timestamp = 2) untouched
        assert_eq!(buf[1].source_timestamp, 2);
    }

    // ----- ring_buf_twap -----

    #[test]
    fn empty_ring_has_no_twap() {
        assert!(ring_buf_twap(&empty_buf(), 1_000).is_none());
    }

    #[test]
    fn twap_uniform_weight_in_window() {
        let mut buf = empty_buf();
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 400_000,
                source_timestamp: 100,
                on_chain_slot: 50,
            },
        );
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 500_000,
                source_timestamp: 200,
                on_chain_slot: 100,
            },
        );
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 600_000,
                source_timestamp: 300,
                on_chain_slot: 150,
            },
        );
        // All three within MAX_STALENESS_SLOTS of now_slot = 200
        let twap = ring_buf_twap(&buf, 200).unwrap();
        assert_eq!(twap, 500_000); // (400 + 500 + 600) / 3 = 500
    }

    #[test]
    fn twap_excludes_stale_entries() {
        let mut buf = empty_buf();
        // Stale entry: age = 1_000 - 25 = 975 > MAX_STALENESS_SLOTS (720)
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 100_000,
                source_timestamp: 50,
                on_chain_slot: 25,
            },
        );
        // Fresh entry: age = 1_000 - 750 = 250 < 720
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 500_000,
                source_timestamp: 100,
                on_chain_slot: 750,
            },
        );
        let twap = ring_buf_twap(&buf, 1_000).unwrap();
        // Only the fresh entry counts
        assert_eq!(twap, 500_000);
    }

    #[test]
    fn twap_returns_none_when_all_stale() {
        let mut buf = empty_buf();
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 500_000,
                source_timestamp: 10,
                on_chain_slot: 5,
            },
        );
        // age = 1_000 - 5 = 995 > 720
        assert!(ring_buf_twap(&buf, 1_000).is_none());
    }

    #[test]
    fn twap_handles_clock_skew_via_saturating_sub() {
        let mut buf = empty_buf();
        // entry.on_chain_slot > now_slot (impossible but defensive)
        ring_buf_push(
            &mut buf,
            OracleSnapshotEntry {
                p_yes_e6: 500_000,
                source_timestamp: 100,
                on_chain_slot: 1_000,
            },
        );
        // saturating_sub keeps age at 0 (in-window); entry counts
        let twap = ring_buf_twap(&buf, 500).unwrap();
        assert_eq!(twap, 500_000);
    }

    #[test]
    fn twap_full_ring_uniform_average() {
        let mut buf = empty_buf();
        // Push 60 entries with linearly varying p_yes_e6 around 500_000,
        // all within the staleness window of now_slot = 100.
        for i in 0..60u64 {
            ring_buf_push(
                &mut buf,
                OracleSnapshotEntry {
                    p_yes_e6: 470_000 + i * 1_000, // 470_000..530_000 in 1_000-bp steps
                    source_timestamp: (i as i64) + 1,
                    on_chain_slot: i + 1,
                },
            );
        }
        // Average of 470_000..529_000 in 1_000-step linear progression
        // is 470_000 + (59 * 1_000) / 2 = 470_000 + 29_500 = 499_500.
        let twap = ring_buf_twap(&buf, 100).unwrap();
        assert_eq!(twap, 499_500);
    }

    // ----- pyth_price_to_p_yes_e6 -----

    /// $100k threshold in e6 USD = 100_000 * 1_000_000.
    const BTC_THRESHOLD_E6: u64 = 100_000_000_000;

    #[test]
    fn formula_at_threshold_returns_midpoint() {
        // delta = 0 => p_change = 0 => 500_000.
        assert_eq!(
            pyth_price_to_p_yes_e6(BTC_THRESHOLD_E6, BTC_THRESHOLD_E6, 100),
            500_000
        );
        assert_eq!(
            pyth_price_to_p_yes_e6(BTC_THRESHOLD_E6, BTC_THRESHOLD_E6, -100),
            500_000
        );
        assert_eq!(
            pyth_price_to_p_yes_e6(BTC_THRESHOLD_E6, BTC_THRESHOLD_E6, i32::MIN),
            500_000
        );
    }

    #[test]
    fn formula_one_percent_above_with_unit_scale() {
        // scale = 100 bps/pct => 1% price move = +100 bps p_yes = +10_000 e6.
        let price = BTC_THRESHOLD_E6 + BTC_THRESHOLD_E6 / 100; // +1%
        assert_eq!(
            pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, 100),
            510_000
        );
    }

    #[test]
    fn formula_one_percent_below_with_unit_scale() {
        // Price 1% below threshold with positive scale => p_yes falls.
        let price = BTC_THRESHOLD_E6 - BTC_THRESHOLD_E6 / 100;
        assert_eq!(
            pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, 100),
            490_000
        );
    }

    #[test]
    fn formula_negative_scale_inverts_direction() {
        // Negative scale: price up => p_yes down. (Market: "price stays below.")
        let price = BTC_THRESHOLD_E6 + BTC_THRESHOLD_E6 / 100;
        assert_eq!(
            pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, -100),
            490_000
        );
    }

    #[test]
    fn formula_clamps_at_extremes() {
        // Massive +price with positive scale should clamp HIGH.
        let price = BTC_THRESHOLD_E6 * 2; // +100% price move
        assert_eq!(
            pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, 10_000),
            POLY_CLAMP_HI
        );
        // Massive -price (well below) should clamp LOW.
        assert_eq!(
            pyth_price_to_p_yes_e6(0, BTC_THRESHOLD_E6, 10_000),
            POLY_CLAMP_LO
        );
    }

    #[test]
    fn formula_zero_threshold_returns_midpoint() {
        // Defensive: never panic on divide-by-zero.
        assert_eq!(pyth_price_to_p_yes_e6(1_000_000, 0, 100), 500_000);
        assert_eq!(pyth_price_to_p_yes_e6(0, 0, i32::MIN), 500_000);
    }

    #[test]
    fn formula_handles_scale_i32_min_without_panic() {
        // i32::MIN is special: its abs() would overflow. We use i128
        // widening (with plain `*`, not wrapping) so this must not
        // panic on any input within the bounded-domain proof.
        let price = BTC_THRESHOLD_E6 + BTC_THRESHOLD_E6 / 100;
        let p = pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, i32::MIN);
        // Extreme negative scale floors the output.
        assert_eq!(p, POLY_CLAMP_LO);
    }

    #[test]
    fn formula_zero_scale_returns_midpoint_everywhere() {
        // scale = 0 makes the numerator identically zero regardless of
        // price/threshold; output is exactly 500_000 for any input.
        // The setter rejects scale = 0, so this is the "what if it
        // slips through" defensive case.
        let prices_to_try = [
            0u64,
            1,
            BTC_THRESHOLD_E6 / 2,
            BTC_THRESHOLD_E6,
            BTC_THRESHOLD_E6 * 2,
            u64::MAX / 2,
        ];
        for &price in &prices_to_try {
            assert_eq!(
                pyth_price_to_p_yes_e6(price, BTC_THRESHOLD_E6, 0),
                500_000,
                "zero scale must return midpoint for price={}",
                price
            );
        }
    }

    #[test]
    fn formula_price_zero_with_positive_scale_clamps_low() {
        // Price = 0 means delta = -threshold, p_change = -scale * 10_000
        // = -100 * 10_000 = -1_000_000. p_signed = 500_000 - 1_000_000
        // = -500_000, clamp pulls it up to POLY_CLAMP_LO.
        assert_eq!(
            pyth_price_to_p_yes_e6(0, BTC_THRESHOLD_E6, 100),
            POLY_CLAMP_LO
        );
        // Same scenario with negative scale floods the output high.
        assert_eq!(
            pyth_price_to_p_yes_e6(0, BTC_THRESHOLD_E6, -100),
            POLY_CLAMP_HI
        );
    }

    // ----- value_deviation_e6 -----

    #[test]
    fn deviation_symmetric_and_bounded() {
        assert_eq!(value_deviation_e6(500_000, 510_000), 10_000);
        assert_eq!(value_deviation_e6(510_000, 500_000), 10_000);
        assert_eq!(value_deviation_e6(500_000, 500_000), 0);
        assert_eq!(value_deviation_e6(0, u64::MAX), u64::MAX);
    }
}
