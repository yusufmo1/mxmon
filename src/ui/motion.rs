//! Fluid graphs: between-tick interpolation of the graph display vectors.
//!
//! Data stays honest — the graph shows the same history over the same
//! timescale as the raw buckets — but the *movement* is continuous. A
//! bucketed graph's body travels exactly one slot per completed bucket
//! (`k` ticks); rather than freezing for `k - 1` ticks and lurching the
//! whole slot inside the completion tick, the body drifts left at
//! **constant velocity**: `1/k` of a slot per tick, advanced smoothly
//! within each tick by the tick phase. Two blended motions:
//!
//! 1. **Conveyor drift** (horizontal, linear) — each rendered slot is a
//!    convex blend of two *adjacent real bucket values* at the current
//!    fractional offset, so the waveform translates instead of jumping.
//! 2. **Eased head** (vertical, ease-out cubic) — a tick that refined the
//!    live head bucket glides its fold from the previous value to the new
//!    one before the drift blend samples it.
//!
//! Every rendered value lies between two real bucket values. The transform
//! layers *in front of* [`Ring::buckets`] — widgets receive an
//! already-interpolated vector and never interpolate themselves
//! ([`crate::app::App::series`] is the only entry point panels use). With
//! motion off, or at the settled instant a bucket completes, the output is
//! bit-identical to `buckets` — which is what keeps golden frames stable.
//! The frame clock lives in `main.rs`: while [`animating`] the recv loop
//! ticks at [`FRAME`] (~30 fps), otherwise it blocks — idle cost stays
//! zero.

use std::time::{Duration, Instant};

use crate::app::{Agg, App, Ring};
use crate::collect::sampler::{POWER_EVERY, TEMPS_EVERY};

/// Frame budget while an animation is in flight (~30 fps).
pub const FRAME: Duration = Duration::from_millis(33);

/// Per-tier arrival stamps, written by `App::apply` as updates land.
#[derive(Debug, Clone, Copy, Default)]
pub struct MotionClock {
    pub fast: Option<Instant>,
    pub power: Option<Instant>,
    pub temps: Option<Instant>,
}

impl MotionClock {
    pub fn last(&self, tier: Tier) -> Option<Instant> {
        match tier {
            Tier::Fast => self.fast,
            Tier::Power => self.power,
            Tier::Temps => self.temps,
        }
    }
}

/// Which sampling tier a graph's ring advances on — its inter-sample
/// interval is the animation's duration. The ping strip is deliberately
/// not here: it's a cell strip on an irregular cadence, not a waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Power,
    Temps,
}

impl Tier {
    /// Inter-sample interval at the live fast-tier setting.
    pub fn interval(self, fast_ms: u64) -> Duration {
        let ms = match self {
            Self::Fast => fast_ms,
            Self::Power => fast_ms * POWER_EVERY,
            Self::Temps => fast_ms * TEMPS_EVERY,
        };
        Duration::from_millis(ms)
    }
}

/// Cubic ease-out, total over any float: non-finite inputs settle to 1.
pub fn ease_out(t: f32) -> f32 {
    if !t.is_finite() {
        return 1.0;
    }
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Fraction of the tier interval elapsed at `now`, clamped to `0..=1`.
/// No stamp yet (source never reported) or a degenerate interval → 1.0,
/// i.e. settled — interpolation only ever runs between two real samples.
pub fn phase(now: Instant, last: Option<Instant>, interval: Duration) -> f32 {
    let Some(last) = last else { return 1.0 };
    let interval = interval.as_secs_f32();
    if interval <= 0.0 {
        return 1.0;
    }
    (now.saturating_duration_since(last).as_secs_f32() / interval).clamp(0.0, 1.0)
}

/// Should the UI loop run its frame clock right now? True only while
/// motion is on, sampling isn't paused, and some graph tier is mid-tick —
/// the moment every phase settles the loop goes back to blocking recv.
pub fn animating(app: &App) -> bool {
    if !app.config.motion || app.paused {
        return false;
    }
    let now = Instant::now();
    [Tier::Fast, Tier::Power, Tier::Temps].into_iter().any(|t| {
        phase(
            now,
            app.motion_clock.last(t),
            t.interval(app.config.interval_ms),
        ) < 1.0
    })
}

/// The interpolated display vector: [`Ring::buckets`] carried by a
/// constant-velocity conveyor. Oldest-first like `buckets`, at most
/// `slots` long, total over any ring contents.
///
/// The drift offset is `(head_n - 1 + phase) / k` slots — the exact
/// fraction of the current bucket that has elapsed — so the body arrives
/// at a full one-slot shift precisely when the bucket completes, and the
/// offset is continuous across every tick boundary (C0: each tick's
/// phase-0 frame equals the previous tick's phase-1 frame). At offset 1
/// (bucket complete, tick settled) the output is bit-identical to
/// `buckets`.
pub fn series(ring: &Ring, slots: usize, k: usize, agg: Agg, phase: f32) -> Vec<f32> {
    let k = k.max(1);
    let ph = if phase.is_finite() {
        phase.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if slots == 0 || ring.pushes() == 0 {
        return ring.buckets(slots, k, agg);
    }
    let head_n = ((ring.pushes() - 1) % k as u64) as usize + 1;
    // Fraction of the current bucket elapsed = fraction of a slot drifted.
    #[allow(clippy::cast_precision_loss)] // head_n <= k <= 8, exact
    let offset = ((head_n - 1) as f32 + ph) / k as f32;
    if offset >= 1.0 {
        return ring.buckets(slots, k, agg);
    }
    // One extra bucket to the left supplies each slot's pre-shift value.
    let mut v = ring.buckets(slots.saturating_add(1), k, agg);
    // Vertical: a refined head eases its fold from the pre-refinement
    // value, so the newest sample lands as a glide, not a step. Either
    // fold non-finite → leave the raw fold (never lerp with NaN).
    if head_n >= 2 {
        let head: Vec<f32> = ring.last_n(head_n).collect();
        // A tiny ring cap can hold fewer samples than the head claims.
        if head.len() >= 2 {
            let prev = agg.fold(&head[..head.len() - 1]);
            if let Some(last) = v.last_mut()
                && prev.is_finite()
                && last.is_finite()
            {
                *last = prev + (*last - prev) * ease_out(ph);
            }
        }
    }
    drift(&v, slots, offset)
}

/// Sample `slots` output columns from `slots + 1` buckets at a fractional
/// leftward offset: each slot is a lerp between its pre-shift value (one
/// column older) and its target. At `offset = 0` this reproduces the
/// bucket-completion frame exactly; at `offset → 1` it approaches the
/// post-shift `buckets` frame. Either neighbor non-finite → snap to the
/// nearer side, never lerp with NaN.
fn drift(v: &[f32], slots: usize, offset: f32) -> Vec<f32> {
    let m = v.len();
    let n = m.min(slots);
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| {
            // Right-aligned: out[i] sits j columns from the right edge.
            let j = n - 1 - i;
            let b = v[m - 1 - j]; // target (post-shift) value
            let a = match (m - 1 - j).checked_sub(1) {
                Some(ai) => v[ai], // pre-shift value: one column older
                None => return b,  // new leftmost column: nothing older
            };
            if a.is_finite() && b.is_finite() {
                a + (b - a) * offset
            } else if offset < 0.5 {
                a
            } else {
                b
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{FRAME, MotionClock, Tier, animating, ease_out, phase, series};
    use crate::app::{Agg, Ring};
    use std::time::{Duration, Instant};

    fn ring_of(vals: &[f32]) -> Ring {
        let mut r = Ring::new(64);
        for &v in vals {
            r.push(v);
        }
        r
    }

    #[test]
    #[allow(clippy::float_cmp)] // clamp endpoints are exact pass-throughs
    fn ease_out_is_total_and_monotone() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-3.0), 0.0);
        assert_eq!(ease_out(7.0), 1.0);
        assert_eq!(ease_out(f32::NAN), 1.0, "non-finite settles");
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = ease_out(i as f32 / 100.0);
            assert!(v >= prev, "monotone");
            prev = v;
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // clamp endpoints are exact pass-throughs
    fn phase_clamps_and_settles() {
        let now = Instant::now();
        let iv = Duration::from_millis(250);
        assert_eq!(phase(now, None, iv), 1.0, "no stamp = settled");
        assert_eq!(phase(now, Some(now), iv), 0.0, "fresh sample");
        let half = now.checked_sub(Duration::from_millis(125)).unwrap();
        assert!((phase(now, Some(half), iv) - 0.5).abs() < 0.01);
        let old = now.checked_sub(Duration::from_secs(9)).unwrap();
        assert_eq!(phase(now, Some(old), iv), 1.0, "stale clamps to settled");
        assert_eq!(phase(now, Some(now), Duration::ZERO), 1.0, "zero interval");
        // A stamp in the future (clock skew) clamps to 0, not negative.
        let future = now + Duration::from_secs(5);
        assert_eq!(phase(now, Some(future), iv), 0.0);
    }

    #[test]
    fn tier_intervals_scale_from_fast() {
        assert_eq!(Tier::Fast.interval(250), Duration::from_millis(250));
        assert_eq!(Tier::Power.interval(250), Duration::from_millis(500));
        assert_eq!(Tier::Temps.interval(250), Duration::from_millis(500));
        assert!(
            FRAME < Tier::Fast.interval(100),
            "frames outpace every tier"
        );
    }

    #[test]
    fn completed_bucket_at_settled_phase_is_identity() {
        // The honesty anchor: the instant a bucket completes and its tick
        // settles, the conveyor offset is exactly 1 and series == buckets.
        for (k, vals) in [
            (1, &[1.0, 5.0, 2.0][..]),
            (2, &[1.0, 5.0, 2.0, 8.0][..]),
            (3, &[1.0, 5.0, 2.0, 8.0, 3.0, 9.0][..]),
            (4, &[1.0, 5.0, 2.0, 8.0, 3.0, 9.0, 4.0, 7.0][..]),
        ] {
            let r = ring_of(vals);
            assert_eq!(r.pushes() % k as u64, 0, "head full by construction");
            for slots in [0, 1, 3, 16] {
                assert_eq!(
                    series(&r, slots, k, Agg::Max, 1.0),
                    r.buckets(slots, k, Agg::Max),
                    "k={k} slots={slots}"
                );
            }
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // C0 endpoints are exact pass-throughs
    fn drift_is_continuous_across_a_refining_tick() {
        // k=4: the 6th push refines the head ([5] → [5, 9]). The new
        // tick's phase-0 frame must equal the old tick's phase-1 frame —
        // same conveyor offset (1/4), and the head fold eased back to its
        // pre-refinement value.
        let before = ring_of(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let after = ring_of(&[1.0, 2.0, 3.0, 4.0, 5.0, 9.0]);
        assert_eq!(
            series(&before, 8, 4, Agg::Max, 1.0),
            series(&after, 8, 4, Agg::Max, 0.0),
            "no boof at the tick boundary"
        );
        // Across the refining tick the head-side column then rises toward
        // the new fold — Max only ever rises within a bucket.
        let mut prev = 0.0;
        for i in 0..=10 {
            let h = series(&after, 8, 4, Agg::Max, i as f32 / 10.0)
                .last()
                .copied()
                .unwrap();
            assert!(h >= prev);
            prev = h;
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // C0 endpoints are exact pass-throughs
    fn drift_is_continuous_across_a_completion_tick() {
        // k=2: the 5th push completes a bucket and starts a fresh head.
        // Phase 0 of the new tick == phase 1 of the old tick (offset 1
        // there, 0 here — the same frame from either side).
        let before = ring_of(&[1.0, 2.0, 3.0, 4.0]);
        let after = ring_of(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let old_settled = series(&before, 3, 2, Agg::Max, 1.0);
        assert_eq!(old_settled, before.buckets(3, 2, Agg::Max), "offset 1");
        let start = series(&after, 3, 2, Agg::Max, 0.0);
        // Right-aligned overlap (the young ring gained a column).
        assert_eq!(start[start.len() - 2..], old_settled[..], "C0 across ticks");
        // Mid-tick every slot sits between its two endpoint frames.
        let end = series(&after, 3, 2, Agg::Max, 1.0);
        let mid = series(&after, 3, 2, Agg::Max, 0.5);
        for (i, m) in mid.iter().enumerate() {
            let (lo, hi) = (start[i].min(end[i]), start[i].max(end[i]));
            assert!((lo..=hi).contains(m), "slot {i} in [{lo}, {hi}]");
        }
    }

    #[test]
    fn velocity_is_constant_across_the_bucket() {
        // k=4 on a linear ramp: the conveyor offset must advance by the
        // same amount for every (head_n, phase) pair that denotes the same
        // elapsed fraction — tick 2 at phase 0.0 == tick 1 at phase 1.0,
        // and mid-bucket frames land strictly between the bucket's
        // endpoint frames in offset order, not bunched into one tick.
        // 16 pushes fill all 4 slots exactly, so every compared frame has
        // the same length (a shorter ring gains a leftmost column mid-test
        // and misaligns the slot-wise comparison).
        let vals: Vec<f32> = (0..16).map(|i| i as f32 * 10.0).collect();
        let full = ring_of(&vals); // 16 pushes, k=4 → head full
        let start = series(&full, 4, 4, Agg::Max, 1.0); // offset 1 == buckets
        // Push one more sample: a new bucket begins, offset re-enters 0.
        let mut r = ring_of(&vals);
        r.push(160.0);
        let quarter = series(&r, 4, 4, Agg::Max, 1.0); // head_n=1, ph=1 → 1/4
        r.push(170.0);
        let half = series(&r, 4, 4, Agg::Max, 1.0); // head_n=2, ph=1 → 2/4
        // On a rising ramp with Max, a greater conveyor offset means every
        // column has drifted further toward its (larger) right neighbor.
        for i in 0..start.len() {
            assert!(start[i] <= quarter[i] && quarter[i] <= half[i], "slot {i}");
        }
        // And the quarter step ≈ half the half step: equal per-tick travel.
        let d1 = quarter[1] - start[1];
        let d2 = half[1] - quarter[1];
        assert!((d1 - d2).abs() < 1e-3, "constant velocity: {d1} vs {d2}");
    }

    #[test]
    fn k1_is_a_conveyor() {
        // Passthrough zoom: every tick completes a bucket, so the offset
        // is the phase itself — at phase 0 the newest sample hasn't
        // visually landed yet.
        let r = ring_of(&[10.0, 20.0, 30.0]);
        assert_eq!(series(&r, 2, 1, Agg::Max, 0.0), vec![10.0, 20.0]);
        assert_eq!(series(&r, 2, 1, Agg::Max, 1.0), vec![20.0, 30.0]);
    }

    #[test]
    fn animating_truth_table() {
        let mut app = crate::testutil::app();
        // Fixture pins motion off for golden determinism.
        assert!(!animating(&app));
        app.config.motion = true;
        // The fixture never stamps the clock through wall time in a way
        // that's fresh — stamp it now to arm, clear to settle.
        app.motion_clock.fast = Some(Instant::now());
        assert!(animating(&app));
        app.paused = true;
        assert!(!animating(&app), "pause stops the frame clock");
        app.paused = false;
        app.motion_clock = MotionClock::default();
        assert!(!animating(&app), "no stamps = settled = blocking recv");
        app.motion_clock.temps = Some(Instant::now());
        assert!(animating(&app), "any tier arms the clock");
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            // Ring soup in, bounded vector out — never panics, never
            // exceeds slots, and a full head at a settled phase is always
            // exactly buckets (the offset-1 identity).
            #[test]
            fn series_is_total_and_bounded(
                vals in proptest::collection::vec(proptest::num::f32::ANY, 0..48),
                slots in 0usize..20,
                k in 0usize..10,
                ph in proptest::num::f32::ANY,
            ) {
                let r = super::ring_of(&vals);
                let out = series(&r, slots, k, Agg::Max, ph);
                prop_assert!(out.len() <= slots.max(1));
                let kk = k.max(1) as u64;
                if r.pushes().is_multiple_of(kk) {
                    let settled = series(&r, slots, k, Agg::Max, 1.0);
                    let expect = r.buckets(slots, kk as usize, Agg::Max);
                    prop_assert_eq!(settled.len(), expect.len());
                    for (a, b) in settled.iter().zip(&expect) {
                        // Bitwise: both sides run the identical buckets
                        // path, and NaN columns (all-non-finite buckets)
                        // must survive — NaN != NaN under float equality.
                        prop_assert!(a.to_bits() == b.to_bits());
                    }
                }
            }

            // C0 continuity at *every* tick boundary — refining and
            // completing alike: each push's phase-0 frame reproduces the
            // previous push's phase-1 frame wherever both display.
            #[test]
            fn every_tick_starts_where_the_last_frame_ended(
                vals in proptest::collection::vec(0.0f32..1000.0, 1..40),
                slots in 1usize..12,
                k in 1usize..6,
            ) {
                let mut r = Ring::new(64);
                let mut prev_frame: Vec<f32> = Vec::new();
                for &v in &vals {
                    r.push(v);
                    if !prev_frame.is_empty() {
                        let start = series(&r, slots, k, Agg::Max, 0.0);
                        // Compare the right-aligned overlap of both frames
                        // (a young ring may have gained a leftmost column
                        // this tick — it has no pre-shift counterpart).
                        let overlap = prev_frame.len().min(start.len());
                        let s_tail = &start[start.len() - overlap..];
                        let p_tail = &prev_frame[prev_frame.len() - overlap..];
                        for (a, b) in s_tail.iter().zip(p_tail) {
                            prop_assert!((a - b).abs() < 1e-3, "{a} vs {b}");
                        }
                    }
                    prev_frame = series(&r, slots, k, Agg::Max, 1.0);
                }
            }
        }
    }
}
