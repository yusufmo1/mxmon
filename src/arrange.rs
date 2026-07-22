//! Where each dashboard card sits — the permutation behind drag-to-rearrange.
//!
//! The layout branches in [`crate::ui::layout`] allocate rects by breakpoint,
//! and those rects are tuned to their position: CPU earns 40% of the top row
//! for its core bands, NET earns a double-height row for the mirrored graph,
//! the heat map is sized from the chassis aspect. None of that should move
//! when a user rearranges the dashboard, so **geometry belongs to the slot and
//! the panel is only a tenant**: an arrangement relabels who draws where and
//! changes no layout math at all. Panels render adaptively into whatever rect
//! they are handed ([`crate::ui::panels`]), which is what makes that legal.
//!
//! Dropping card A on card B swaps exactly those two — every other card stays
//! put. Swaps compose, so the state is a bijection over [`PANELS`], stored as
//! "which panel draws in each home position". The identity is the shipped
//! layout, and rendering under it is bit-for-bit what mxmon drew before this
//! module existed.

use ratatui::layout::Rect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ui::widgets::PanelKind;

/// Every rearrangeable card, in slot order: index `i` is the home position of
/// `PANELS[i]`. This is also the serialization order, so it must stay stable.
pub const PANELS: [PanelKind; 10] = [
    PanelKind::Cpu,
    PanelKind::Power,
    PanelKind::Battery,
    PanelKind::Gpu,
    PanelKind::Mem,
    PanelKind::Net,
    PanelKind::Disk,
    PanelKind::Temps,
    PanelKind::HeatMap,
    PanelKind::Procs,
];

/// Which panel is drawn in each home position.
///
/// `slots[i]` is the panel that draws where `PANELS[i]` normally would. A
/// bijection over [`PANELS`] by construction: the only mutator is
/// [`swap`](Arrangement::swap), and the only way in from outside is a
/// deserializer that rejects anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    slots: [PanelKind; PANELS.len()],
}

impl Default for Arrangement {
    fn default() -> Self {
        Self { slots: PANELS }
    }
}

impl Arrangement {
    /// The panel that draws in `slot`'s home position.
    ///
    /// A `PanelKind` that is not rearrangeable (there are none today, but the
    /// enum is open to growth) maps to itself, so callers stay total.
    pub fn at(&self, slot: PanelKind) -> PanelKind {
        match PANELS.iter().position(|&k| k == slot) {
            Some(i) => self.slots[i],
            None => slot,
        }
    }

    /// Which home position `shown` currently occupies.
    fn slot_of(&self, shown: PanelKind) -> Option<usize> {
        self.slots.iter().position(|&k| k == shown)
    }

    /// Trade the positions of two cards, named by what is *displayed* on them
    /// — which is what a drag or the arrange cursor hands us. Looking each one
    /// up by its current slot is what lets swaps compose: after `cpu ⇄ gpu`,
    /// grabbing "the GPU card" grabs the slot CPU used to own.
    ///
    /// Swapping a card with itself, or with anything not in [`PANELS`], is a
    /// no-op rather than an error: both are reachable from a stale hit rect.
    pub fn swap(&mut self, a: PanelKind, b: PanelKind) {
        let (Some(i), Some(j)) = (self.slot_of(a), self.slot_of(b)) else {
            return;
        };
        self.slots.swap(i, j);
    }

    /// Whether every card is still in its shipped position.
    pub fn is_default(&self) -> bool {
        self.slots == PANELS
    }

    /// How many cards sit somewhere other than their home position — the
    /// number the settings row reports.
    pub fn moved(&self) -> usize {
        self.slots
            .iter()
            .zip(PANELS)
            .filter(|&(&shown, home)| shown != home)
            .count()
    }

    /// Build from a list of panel names, or fall back to the identity.
    ///
    /// Total by design: duplicates, unknown names, and a wrong length all
    /// yield the default. An arrangement that is not a bijection would hide
    /// one card and draw another twice, which is worse than ignoring a
    /// hand-edited value.
    fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        if names.len() != PANELS.len() {
            return Self::default();
        }
        let mut slots = [PanelKind::Cpu; PANELS.len()];
        for (slot, name) in slots.iter_mut().zip(names) {
            match PanelKind::parse(name.as_ref()) {
                Some(kind) => *slot = kind,
                None => return Self::default(),
            }
        }
        // Every panel exactly once, or it is not a bijection.
        if PANELS.iter().any(|k| !slots.contains(k)) {
            return Self::default();
        }
        Self { slots }
    }

    fn names(&self) -> Vec<&'static str> {
        self.slots.iter().map(|k| k.name()).collect()
    }
}

/// Which way the arrange cursor is being pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

fn center(r: Rect) -> (i32, i32) {
    (
        i32::from(r.x) * 2 + i32::from(r.width),
        i32::from(r.y) * 2 + i32::from(r.height),
    )
}

/// The card the cursor lands on when pushed `dir` from `from`.
///
/// `cards` is what the last frame actually laid out (read back off the hit
/// map), so the cursor walks exactly what the user can see — there is no
/// second copy of the layout to drift out of sync.
///
/// Candidates are the cards whose centre lies beyond `from`'s in `dir`, and
/// the ones that still *overlap* it on the cross axis win outright. That
/// ranking is the whole trick: cards are wildly different sizes here, so a
/// plain nearest-centre rule sends `→` from the 88-column CPU card diagonally
/// into the row below, whose centre happens to be closer than its actual
/// neighbour's. Overlap first, then nearest along the axis, then nearest
/// across it. `None` when nothing lies that way — the cursor stops at the
/// edge rather than wrapping.
pub fn step(cards: &[(Rect, PanelKind)], from: PanelKind, dir: Dir) -> Option<PanelKind> {
    let source = cards.iter().find(|&&(_, k)| k == from).map(|&(r, _)| r)?;
    let (fx, fy) = center(source);
    cards
        .iter()
        .filter(|&&(_, k)| k != from)
        .filter_map(|&(r, k)| {
            let (cx, cy) = center(r);
            let beyond = match dir {
                Dir::Left => cx < fx,
                Dir::Right => cx > fx,
                Dir::Up => cy < fy,
                Dir::Down => cy > fy,
            };
            if !beyond {
                return None;
            }
            // Sharing rows (for a horizontal step) or columns (for a vertical
            // one) is what "the card next to this one" means.
            let aligned = match dir {
                Dir::Left | Dir::Right => r.top() < source.bottom() && source.top() < r.bottom(),
                Dir::Up | Dir::Down => r.left() < source.right() && source.left() < r.right(),
            };
            let (primary, cross) = match dir {
                Dir::Left | Dir::Right => ((cx - fx).abs(), (cy - fy).abs()),
                Dir::Up | Dir::Down => ((cy - fy).abs(), (cx - fx).abs()),
            };
            Some(((!aligned, primary, cross), k))
        })
        // Ties go to the earliest card laid out, so stepping is deterministic
        // rather than dependent on iteration order.
        .min_by_key(|&(score, _)| score)
        .map(|(_, k)| k)
}

impl Serialize for Arrangement {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.names().serialize(s)
    }
}

/// **Infallible on purpose.** `Config::load` parses the whole file in one
/// `toml::from_str(..).ok()`, so a field that *errors* would discard every
/// other setting with it. A malformed arrangement degrades to the shipped
/// layout and nothing else — the same posture as [`crate::history`].
impl<'de> Deserialize<'de> for Arrangement {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Anything that is not a list of strings (a number, a table, a
        // truncated list) lands here as `None` rather than an error.
        let raw = Option::<Vec<String>>::deserialize(d).unwrap_or_default();
        Ok(raw.map_or_else(Self::default, |names| Self::from_names(&names)))
    }
}

#[cfg(test)]
mod tests {
    use super::{Arrangement, PANELS, step};
    use crate::ui::widgets::PanelKind as P;
    use ratatui::layout::Rect;

    #[test]
    fn identity_is_the_shipped_layout() {
        let a = Arrangement::default();
        assert!(a.is_default());
        assert_eq!(a.moved(), 0);
        for kind in PANELS {
            assert_eq!(a.at(kind), kind, "{kind:?} starts at home");
        }
    }

    #[test]
    fn swap_trades_exactly_two_cards() {
        let mut a = Arrangement::default();
        a.swap(P::Cpu, P::Gpu);
        assert_eq!(a.at(P::Cpu), P::Gpu, "the CPU slot now draws GPU");
        assert_eq!(a.at(P::Gpu), P::Cpu);
        assert_eq!(a.moved(), 2, "and nothing else moved");
        for kind in PANELS.into_iter().filter(|k| !matches!(k, P::Cpu | P::Gpu)) {
            assert_eq!(a.at(kind), kind);
        }
        // Involution: the same swap puts it back.
        a.swap(P::Gpu, P::Cpu);
        assert!(a.is_default());
    }

    #[test]
    fn swaps_compose_by_what_is_displayed() {
        // After cpu ⇄ gpu, grabbing "the GPU card" grabs the slot CPU owned.
        let mut a = Arrangement::default();
        a.swap(P::Cpu, P::Gpu);
        a.swap(P::Gpu, P::Mem);
        // GPU left the CPU slot for the MEM slot; MEM took the CPU slot.
        assert_eq!(a.at(P::Cpu), P::Mem);
        assert_eq!(a.at(P::Mem), P::Gpu);
        assert_eq!(
            a.at(P::Gpu),
            P::Cpu,
            "CPU stayed where the first swap put it"
        );
        assert_eq!(a.moved(), 3);
    }

    #[test]
    fn swapping_a_card_with_itself_does_nothing() {
        let mut a = Arrangement::default();
        a.swap(P::Cpu, P::Cpu);
        assert!(a.is_default());
    }

    /// Every reachable sequence of swaps keeps the bijection: each panel is
    /// drawn exactly once, so no card can vanish or appear twice.
    #[test]
    fn any_swap_sequence_stays_a_bijection() {
        let mut a = Arrangement::default();
        for (i, &x) in PANELS.iter().enumerate() {
            for &y in &PANELS[i..] {
                a.swap(x, y);
                for kind in PANELS {
                    assert_eq!(
                        a.slots.iter().filter(|&&k| k == kind).count(),
                        1,
                        "{kind:?} drawn exactly once after swapping {x:?}/{y:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn names_round_trip_through_parse() {
        for kind in PANELS {
            assert_eq!(P::parse(kind.name()), Some(kind), "{kind:?}");
        }
        assert_eq!(P::parse("nonsense"), None);
        assert_eq!(P::parse(""), None);
    }

    #[test]
    fn malformed_name_lists_fall_back_to_the_identity() {
        let ok: Vec<&str> = PANELS.iter().map(|k| k.name()).collect();
        assert!(Arrangement::from_names(&ok).is_default());

        let mut dupe = ok.clone();
        dupe[1] = "cpu"; // cpu twice, power missing
        assert!(Arrangement::from_names(&dupe).is_default(), "duplicates");

        let mut unknown = ok.clone();
        unknown[0] = "quantum";
        assert!(
            Arrangement::from_names(&unknown).is_default(),
            "unknown name"
        );

        assert!(Arrangement::from_names(&ok[..3]).is_default(), "too short");
        let mut long = ok.clone();
        long.push("cpu");
        assert!(Arrangement::from_names(&long).is_default(), "too long");
        assert!(
            Arrangement::from_names::<&str>(&[]).is_default(),
            "empty list"
        );

        // A real permutation still survives.
        let mut swapped = ok.clone();
        swapped.swap(0, 3);
        assert_eq!(Arrangement::from_names(&swapped).at(P::Cpu), P::Gpu);
    }

    #[test]
    fn deserialize_tolerates_anything_toml_can_hold() {
        #[derive(serde::Deserialize)]
        struct Holder {
            #[serde(default)]
            arrangement: Arrangement,
        }
        // Each of these is a value a hand-edited config could contain; none
        // may error, because an error would discard the whole config file.
        for src in [
            "",
            "arrangement = 7",
            "arrangement = \"cpu\"",
            "arrangement = []",
            "arrangement = [\"cpu\", \"cpu\"]",
            "arrangement = [1, 2, 3]",
            "arrangement = { cpu = \"gpu\" }",
            "arrangement = [\"cpu\", \"power\", \"battery\", \"gpu\", \"mem\", \"net\", \"disk\", \"temps\", \"heat\", \"nope\"]",
        ] {
            let h: Holder = toml::from_str(src).unwrap_or_else(|e| panic!("{src:?} errored: {e}"));
            assert!(h.arrangement.is_default(), "{src:?} should fall back");
        }
        // …and a well-formed one is honored.
        let h: Holder = toml::from_str(
            "arrangement = [\"gpu\", \"power\", \"battery\", \"cpu\", \"mem\", \"net\", \"disk\", \"temps\", \"heat\", \"procs\"]",
        )
        .expect("valid");
        assert_eq!(h.arrangement.at(P::Cpu), P::Gpu);
        assert_eq!(h.arrangement.at(P::Gpu), P::Cpu);
    }

    /// A 2×2 grid of cards, laid out the way a frame would hand them over.
    fn grid() -> Vec<(Rect, P)> {
        vec![
            (Rect::new(0, 0, 20, 10), P::Cpu),
            (Rect::new(20, 0, 20, 10), P::Power),
            (Rect::new(0, 10, 20, 10), P::Gpu),
            (Rect::new(20, 10, 20, 10), P::Mem),
        ]
    }

    #[test]
    fn the_cursor_steps_to_the_neighbour_in_each_direction() {
        let g = grid();
        use super::Dir::{Down, Left, Right, Up};
        assert_eq!(step(&g, P::Cpu, Right), Some(P::Power));
        assert_eq!(step(&g, P::Cpu, Down), Some(P::Gpu));
        assert_eq!(step(&g, P::Mem, Left), Some(P::Gpu));
        assert_eq!(step(&g, P::Mem, Up), Some(P::Power));
        // The diagonal is never preferred over the card beside you.
        assert_eq!(step(&g, P::Gpu, Right), Some(P::Mem));
        assert_eq!(step(&g, P::Power, Down), Some(P::Mem));
    }

    #[test]
    fn the_cursor_stops_at_the_edges() {
        let g = grid();
        use super::Dir::{Down, Left, Right, Up};
        assert_eq!(step(&g, P::Cpu, Left), None);
        assert_eq!(step(&g, P::Cpu, Up), None);
        assert_eq!(step(&g, P::Mem, Right), None);
        assert_eq!(step(&g, P::Mem, Down), None);
    }

    /// The real ≥130-column overview at 200×50, where the cards are very
    /// different sizes: an 88-wide CPU card over a row of 40-wide ones. By
    /// nearest centre alone, `→` from CPU lands on MEM in the row *below*
    /// (its centre is 32 away, POWER's is 140) — which is why alignment has
    /// to outrank distance.
    #[test]
    fn a_wide_card_steps_to_its_real_neighbour_not_a_nearer_diagonal() {
        let cards = vec![
            (Rect::new(0, 1, 88, 12), P::Cpu),
            (Rect::new(88, 1, 52, 12), P::Power),
            (Rect::new(140, 1, 60, 12), P::Battery),
            (Rect::new(0, 13, 40, 16), P::Gpu),
            (Rect::new(40, 13, 40, 16), P::Mem),
            (Rect::new(80, 13, 40, 16), P::Net),
            (Rect::new(120, 13, 40, 16), P::Disk),
            (Rect::new(160, 13, 40, 16), P::Temps),
            (Rect::new(0, 29, 140, 20), P::Procs),
            (Rect::new(140, 29, 60, 20), P::HeatMap),
        ];
        use super::Dir::{Down, Left, Right, Up};
        assert_eq!(step(&cards, P::Cpu, Right), Some(P::Power));
        assert_eq!(step(&cards, P::Power, Right), Some(P::Battery));
        assert_eq!(step(&cards, P::Battery, Left), Some(P::Power));
        // Down from the top row reaches the metric row, not past it.
        assert_eq!(step(&cards, P::Power, Down), Some(P::Net));
        assert_eq!(step(&cards, P::Gpu, Right), Some(P::Mem));
        assert_eq!(step(&cards, P::Temps, Down), Some(P::HeatMap));
        // Up from the wide table lands on the metric card nearest its centre
        // (x=70), which is MEM — not simply the leftmost one.
        assert_eq!(step(&cards, P::Procs, Up), Some(P::Mem));
        assert_eq!(step(&cards, P::Cpu, Up), None);
    }

    #[test]
    fn stepping_is_total_against_a_stale_cursor() {
        // The cursor names a card the frame no longer lays out (it was hidden,
        // or the view changed under it) — that is a dead end, not a panic.
        let g = grid();
        assert_eq!(step(&g, P::Procs, super::Dir::Right), None);
        assert_eq!(step(&[], P::Cpu, super::Dir::Down), None);
        // Degenerate rects are legal input too.
        let zero = [
            (Rect::new(0, 0, 0, 0), P::Cpu),
            (Rect::new(0, 0, 0, 0), P::Gpu),
        ];
        assert_eq!(step(&zero, P::Cpu, super::Dir::Right), None);
    }

    #[test]
    fn serialize_round_trips() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Holder {
            arrangement: Arrangement,
        }
        let mut a = Arrangement::default();
        a.swap(P::Procs, P::HeatMap);
        a.swap(P::Cpu, P::Temps);
        let toml = toml::to_string(&Holder {
            arrangement: a.clone(),
        })
        .expect("serialize");
        let back: Holder = toml::from_str(&toml).expect("deserialize");
        assert_eq!(back.arrangement, a);
    }
}
