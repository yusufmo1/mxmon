//! Cross-run graph history: `~/.config/mxmon/history.bin`.
//!
//! macOS publishes no historical telemetry to a sudoless process — every
//! source is either an instantaneous gauge (memory, SMC temperatures, GPU
//! residency) or a counter cumulative since boot, which yields exactly one
//! number rather than a series. So the only honest way to open with a
//! populated graph is to restore what mxmon itself recorded: rings are
//! written on quit and read back at startup.
//!
//! The time the app was closed is pushed back as [`crate::app::UNOBSERVED`]
//! samples, which render as absence
//! ([`crate::ui::widgets::Slot::Uncovered`]) rather than as a gap in data
//! that was being watched. That distinction is what keeps this file honest
//! at every graph width with no threshold to tune: a short absence shows the
//! restored run with a break at its right, and a long one scrolls the run
//! off entirely and leaves the graph blank. Marking the gap as a NaN instead
//! would paint a full-width dotted floor the moment the absence outgrew a
//! panel's visible window — the "pinned at zero" reading the `Slot`
//! distinction exists to prevent.

use std::path::PathBuf;

use crate::app::{HISTORY, Histories, Ring};
use crate::collect::sampler::{PING_EVERY, POWER_EVERY, TEMPS_EVERY};
use crate::collect::soc::SocInfo;

/// Magic and format version in one token: a format change bumps the trailing
/// digit, every older file stops being recognized, and the stale data is
/// dropped. There is no version-negotiation path to get wrong.
const MAGIC: [u8; 8] = *b"MXMONHS1";

/// Fingerprint cap — long enough for any chip name, short enough that a
/// corrupt length can't describe a huge read.
const CHIP_MAX: usize = 255;

/// Number of rings in [`persisted`]; the file carries its own count, so this
/// is only the array size.
const PERSISTED: usize = 21;

/// Every ring that round-trips, as `(stable tag, fast ticks per sample,
/// ring)`.
///
/// Tags are permanent: reordering this list is safe, but changing a tag
/// orphans that ring's saved data (it simply stops restoring). The tick
/// multiplier is the ring's own tier cadence — it's what turns "closed for
/// 40 s" into the right number of missing samples for *that* ring, since
/// the power and temps tiers advance at half the fast rate and ping at a
/// quarter.
///
/// `per_core` is deliberately absent: no panel renders it, and its length
/// varies with the machine.
fn persisted(h: &mut Histories) -> [(u16, u64, &mut Ring); PERSISTED] {
    [
        // Fast tier — one sample per fast tick.
        (1, 1, &mut h.cpu_total),
        (2, 1, &mut h.gpu),
        (3, 1, &mut h.mem_used),
        (4, 1, &mut h.net_rx),
        (5, 1, &mut h.net_tx),
        (6, 1, &mut h.disk_rd),
        (7, 1, &mut h.disk_wr),
        // Power tier.
        (8, POWER_EVERY, &mut h.package_w),
        (9, POWER_EVERY, &mut h.cpu_w),
        (10, POWER_EVERY, &mut h.gpu_w),
        (11, POWER_EVERY, &mut h.ane_w),
        (12, POWER_EVERY, &mut h.dram_w),
        (20, POWER_EVERY, &mut h.amcc_w),
        (21, POWER_EVERY, &mut h.dcs_w),
        (13, POWER_EVERY, &mut h.disp_w),
        (14, POWER_EVERY, &mut h.ecpu_usage),
        (15, POWER_EVERY, &mut h.pcpu_usage),
        // Temps tier (the SMC sweep also carries system power).
        (16, TEMPS_EVERY, &mut h.cpu_temp),
        (17, TEMPS_EVERY, &mut h.gpu_temp),
        (18, TEMPS_EVERY, &mut h.sys_w),
        // Ping runs on its own thread at its own multiple.
        (19, PING_EVERY, &mut h.ping_ms),
    ]
}

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("history.bin"))
}

/// Bytes identifying the machine the samples came from. A config dir carried
/// to another Mac restores nothing rather than grafting one chip's history
/// onto another's graphs.
fn fingerprint(soc: &SocInfo) -> &[u8] {
    let b = soc.chip_name.as_bytes();
    &b[..b.len().min(CHIP_MAX)]
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- wire format ---------------------------------------------------------
//
// magic[8] · saved_at u64 · chip_len u8 · chip[chip_len] · ring_count u16
//   then per ring: tag u16 · len u32 · samples[len] f32
//
// All little-endian. Every read below is bounds-checked and returns `None`
// on a short or hostile buffer — a truncated or corrupt file degrades to
// "no history", never to a panic.

/// Bounds-checked sequential reader over a byte buffer.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// A decoded file: when it was written, which machine wrote it, and the
/// samples per ring tag.
struct Saved {
    saved_at: u64,
    chip: Vec<u8>,
    rings: Vec<(u16, Vec<f32>)>,
}

fn encode(h: &mut Histories, soc: &SocInfo, saved_at: u64) -> Vec<u8> {
    let chip = fingerprint(soc);
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&saved_at.to_le_bytes());
    out.push(chip.len() as u8);
    out.extend_from_slice(chip);
    out.extend_from_slice(&(PERSISTED as u16).to_le_bytes());
    for (tag, _, ring) in persisted(h) {
        let vals: Vec<f32> = ring.last_n(HISTORY).collect();
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&(vals.len() as u32).to_le_bytes());
        for v in vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<Saved> {
    let mut c = Cur { b: bytes, at: 0 };
    if c.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let saved_at = c.u64()?;
    let chip_len = c.u8()? as usize;
    let chip = c.take(chip_len)?.to_vec();
    let count = c.u16()?;
    let mut rings = Vec::new();
    for _ in 0..count {
        let tag = c.u16()?;
        let len = c.u32()? as usize;
        // The samples must actually be present before anything is
        // allocated, so a bogus length can't turn into a huge reservation.
        let raw = c.take(len.checked_mul(4)?)?;
        let vals = raw
            .chunks_exact(4)
            .filter_map(|b| b.try_into().ok().map(f32::from_le_bytes))
            .collect();
        rings.push((tag, vals));
    }
    Some(Saved {
        saved_at,
        chip,
        rings,
    })
}

// ---- public API ----------------------------------------------------------

/// Best-effort persist of every ring (a read-only home dir must not break
/// quitting). Takes `&mut` only because [`persisted`] is the single
/// definition of the tag→ring table; nothing is mutated.
pub fn save(h: &mut Histories, soc: &SocInfo, now: u64) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, encode(h, soc, now));
}

/// Refill rings from the last run, then push the closed-app interval back as
/// non-finite samples so the break renders as a break. Returns how many
/// rings were restored (0 for a missing, corrupt, foreign, or stale file).
///
/// `now` is passed in rather than read here so the staleness rules are
/// testable without a clock.
pub fn restore(h: &mut Histories, soc: &SocInfo, fast_ms: u64, now: u64) -> usize {
    let Some(path) = path() else { return 0 };
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let Some(saved) = decode(&bytes) else {
        return 0;
    };
    if saved.chip != fingerprint(soc) {
        return 0;
    }
    // A file written "in the future" (clock skew) reads as no gap at all
    // rather than an absurd one.
    let gap_secs = now.saturating_sub(saved.saved_at);
    let mut restored = 0;
    for (tag, every, ring) in persisted(h) {
        let Some((_, vals)) = saved.rings.iter().find(|(t, _)| *t == tag) else {
            continue;
        };
        if vals.is_empty() {
            continue;
        }
        let step_ms = fast_ms.saturating_mul(every).max(1);
        // Samples the ring would have taken while nothing was running. No
        // threshold guards this: the gap is pushed as UNOBSERVED, which
        // draws as absence at every width, so a long one simply scrolls the
        // restored run off the graph and leaves it blank — the honest
        // picture — instead of a full-width dotted floor. Capping at the
        // ring's own capacity just avoids pushing samples that would be
        // evicted anyway.
        let gap_ticks = (gap_secs.saturating_mul(1000) / step_ms).min(HISTORY as u64);
        for v in vals {
            ring.push(*v);
        }
        for _ in 0..gap_ticks {
            ring.push(crate::app::UNOBSERVED);
        }
        restored += 1;
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::{HISTORY, MAGIC, PERSISTED, decode, encode, persisted, restore, save};
    use crate::app::{Histories, is_unobserved};
    use crate::collect::sampler::POWER_EVERY;
    use crate::config::test_dir;
    use crate::testutil as tu;
    use crate::ui::widgets::{Slot, slot_at};

    fn socinfo() -> crate::collect::soc::SocInfo {
        tu::soc()
    }

    /// A Histories with a known ramp in every persisted ring.
    fn seeded(n: usize) -> Histories {
        let mut h = Histories::new(4);
        for (_, _, ring) in persisted(&mut h) {
            for i in 0..n {
                ring.push(i as f32);
            }
        }
        h
    }

    #[test]
    fn tags_are_unique_and_cadences_sane() {
        let mut h = Histories::new(4);
        let table = persisted(&mut h);
        assert_eq!(table.len(), PERSISTED);
        let mut tags: Vec<u16> = table.iter().map(|(t, _, _)| *t).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), PERSISTED, "every tag must be distinct");
        let mut h = Histories::new(4);
        assert!(
            persisted(&mut h).iter().all(|(_, every, _)| *every >= 1),
            "a zero cadence would divide by zero in the gap math"
        );
    }

    #[test]
    fn round_trips_every_ring() {
        let mut h = seeded(10);
        let bytes = encode(&mut h, &socinfo(), 1_000);
        let saved = decode(&bytes).expect("decodes");
        assert_eq!(saved.saved_at, 1_000);
        assert_eq!(saved.chip, socinfo().chip_name.as_bytes());
        assert_eq!(saved.rings.len(), PERSISTED);
        // Bit-for-bit: the encoding has to be lossless, so exact equality is
        // the property under test (compared as bits to say so precisely, and
        // because it's the only comparison that also pins NaN gaps).
        let want: Vec<u32> = (0..10).map(|i| f32::to_bits(i as f32)).collect();
        for (tag, vals) in &saved.rings {
            let got: Vec<u32> = vals.iter().copied().map(f32::to_bits).collect();
            assert_eq!(got, want, "ring {tag} round-trips unchanged");
        }
    }

    #[test]
    fn decode_rejects_junk_without_panicking() {
        assert!(decode(&[]).is_none(), "empty");
        assert!(decode(b"not-mxmon-at-all").is_none(), "wrong magic");
        // Right magic, truncated everywhere after it.
        let mut h = seeded(4);
        let full = encode(&mut h, &socinfo(), 1);
        for cut in 0..full.len() {
            let _ = decode(&full[..cut]); // must not panic
        }
        assert!(decode(&full).is_some(), "the whole buffer still decodes");
        // A ring claiming more samples than the file holds.
        let mut lying = full.clone();
        let n = lying.len();
        lying[n - 1] = 0xFF;
        let _ = decode(&lying);
    }

    #[test]
    fn restore_fills_the_gap_at_each_tier_cadence() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        let soc = socinfo();
        let mut h = seeded(10);
        save(&mut h, &soc, 1_000);

        // Closed for 4 s at a 250 ms fast tick: 16 fast ticks missed, but
        // only 8 on the power tier and 4 on ping.
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &soc, 250, 1_004), PERSISTED);
        let fast: Vec<f32> = fresh.cpu_total.last_n(usize::MAX).collect();
        assert_eq!(fast.len(), 10 + 16);
        assert!(fast[..10].iter().all(|v| v.is_finite()), "saved data");
        assert!(
            fast[10..].iter().copied().all(is_unobserved),
            "the closed interval, marked as unobserved rather than missing"
        );
        let power: Vec<f32> = fresh.package_w.last_n(usize::MAX).collect();
        assert_eq!(power.len(), 10 + 16 / POWER_EVERY as usize);
        let ping: Vec<f32> = fresh.ping_ms.last_n(usize::MAX).collect();
        assert_eq!(ping.len(), 10 + 4);
    }

    #[test]
    fn restore_drops_stale_foreign_and_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        let soc = socinfo();

        // Nothing saved yet.
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &soc, 250, 1_000), 0);
        assert!(fresh.cpu_total.is_empty());

        let mut h = seeded(10);
        save(&mut h, &soc, 1_000);

        // Away long enough to scroll the saved run clean off the ring: the
        // data is gone and every remaining sample is unobserved, so the
        // graph draws nothing at all. No threshold decides this — the ring's
        // own eviction does, which is why it holds at any absence length.
        let mut fresh = Histories::new(4);
        let ages = 1_000 + HISTORY as u64 * 250 / 1000 + 60;
        restore(&mut fresh, &soc, 250, ages);
        let fast: Vec<f32> = fresh.cpu_total.last_n(usize::MAX).collect();
        assert!(
            fast.iter().copied().all(is_unobserved),
            "a long absence leaves unobserved time, never a dotted floor"
        );
        assert!(
            !fast.iter().any(|v| v.is_nan()),
            "and never a NaN, which would draw as a gap in watched data"
        );

        // A different machine's file is ignored.
        let mut other = socinfo();
        other.chip_name = "Apple M99 Ultra".into();
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &other, 250, 1_004), 0);

        // A clock that ran backwards reads as no gap, not a huge one.
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &soc, 250, 1), PERSISTED);
        assert_eq!(fresh.cpu_total.last_n(usize::MAX).count(), 10);

        // Corrupt file → no history, no panic.
        std::fs::write(tmp.path().join("history.bin"), b"\xff\xff\xff").unwrap();
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &soc, 250, 1_004), 0);
    }

    /// The regression a 23-minute relaunch shipped: the closed interval was
    /// marked with NaN, so once it outgrew a panel's visible window every
    /// drawn column classified as [`Slot::Gap`] and the graph became a
    /// full-width dotted floor — the "pinned at zero" reading the `Slot`
    /// split exists to prevent. Marking it unobserved instead is what makes
    /// this hold at *every* width and absence, with nothing to tune.
    ///
    /// The invariant is not "some data survives" — for a long absence there
    /// is honestly nothing to show. It is that a restored gap never draws as
    /// a gap in watched data.
    #[test]
    fn a_restored_gap_never_draws_as_a_floor_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        let soc = socinfo();
        let mut h = seeded(2_000);
        save(&mut h, &soc, 1_000);

        // 50 columns at ×4 is ~400 samples; narrow panels show fewer still.
        for window in [1, 100, 400, 800] {
            for away in [1, 30, 60, 240, 600, 4_000, 100_000] {
                let mut fresh = Histories::new(4);
                restore(&mut fresh, &soc, 250, 1_000 + away);
                for (tag, _, ring) in persisted(&mut fresh) {
                    let seen: Vec<f32> = ring.last_n(window).collect();
                    for slot in 0..window {
                        assert_ne!(
                            slot_at(&seen, window, slot),
                            Slot::Gap,
                            "ring {tag}: {away}s away drew a dotted floor in \
                             a {window}-sample window"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn without_a_config_dir_save_and_restore_are_silent_no_ops() {
        // The hermetic default: no override installed, so nothing resolves.
        let soc = socinfo();
        let mut h = seeded(4);
        save(&mut h, &soc, 1_000);
        let mut fresh = Histories::new(4);
        assert_eq!(restore(&mut fresh, &soc, 250, 1_004), 0);
    }

    mod prop {
        use proptest::prelude::*;

        proptest! {
            // The file is user-writable and survives crashes mid-write, so
            // the decoder meets arbitrary bytes. It must always terminate
            // with a value or None — never a panic, never a huge alloc.
            #[test]
            fn decode_is_total_over_arbitrary_bytes(
                bytes in proptest::collection::vec(any::<u8>(), 0..512),
            ) {
                let _ = super::super::decode(&bytes);
            }

            // The same, but past the magic check, where the length-driven
            // reads actually run.
            #[test]
            fn decode_is_total_past_the_magic(
                tail in proptest::collection::vec(any::<u8>(), 0..512),
            ) {
                let mut buf = super::MAGIC.to_vec();
                buf.extend_from_slice(&tail);
                let _ = super::super::decode(&buf);
            }
        }
    }
}
