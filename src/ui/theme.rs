//! Themes are pure data: semantic color roles + gradient stops. Panels never
//! reference literal colors.

use std::sync::OnceLock;

use ratatui::style::Color;

/// Whether the terminal renders 24-bit SGR. Terminal.app famously does not —
/// without a fallback every RGB span would drop to the default color there.
pub fn truecolor_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return true;
        }
        // Apple Terminal never supports truecolor; most others do by now.
        std::env::var("TERM_PROGRAM").map_or(true, |p| p != "Apple_Terminal")
    })
}

/// Quantize an RGB color onto the xterm-256 palette (6×6×6 cube + gray ramp).
pub fn to_indexed(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    let scale = |v: u8| -> u8 {
        // Cube levels: 0, 95, 135, 175, 215, 255.
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let (cr, cg, cb) = (scale(r), scale(g), scale(b));
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let level = |c: u8| if c == 0 { 0i32 } else { i32::from(c) * 40 + 55 };
    let cube_dist = (i32::from(r) - level(cr)).pow(2)
        + (i32::from(g) - level(cg)).pow(2)
        + (i32::from(b) - level(cb)).pow(2);

    // Grayscale ramp 232..=255: 8 + 10n.
    let avg = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let gray_n = ((avg.saturating_sub(8)) / 10).min(23) as i32;
    let gray_level = 8 + 10 * gray_n;
    let gray_dist = (i32::from(r) - gray_level).pow(2)
        + (i32::from(g) - gray_level).pow(2)
        + (i32::from(b) - gray_level).pow(2);

    if gray_dist < cube_dist {
        Color::Indexed((232 + gray_n) as u8)
    } else {
        Color::Indexed(cube_idx)
    }
}

/// A color ramp; `at(t)` maps t in 0..=1 to a color.
#[derive(Debug, Clone, Copy)]
pub enum Gradient {
    /// Multi-stop RGB interpolation.
    Stops(&'static [(f32, (u8, u8, u8))]),
    /// A single color at every t (for series that shouldn't shift hue).
    Solid(Color),
}

impl Gradient {
    pub const fn new(stops: &'static [(f32, (u8, u8, u8))]) -> Self {
        Self::Stops(stops)
    }

    pub fn at(&self, t: f32) -> Color {
        let stops = match self {
            Self::Solid(color) => return *color,
            Self::Stops(stops) => stops,
        };
        let t = t.clamp(0.0, 1.0);
        let mut prev = stops[0];
        for &stop in *stops {
            if t <= stop.0 {
                let span = (stop.0 - prev.0).max(f32::EPSILON);
                let local = (t - prev.0) / span;
                let (r0, g0, b0) = prev.1;
                let (r1, g1, b1) = stop.1;
                return Color::Rgb(
                    lerp(r0, r1, local),
                    lerp(g0, g1, local),
                    lerp(b0, b1, local),
                );
            }
            prev = stop;
        }
        let (r, g, b) = stops[stops.len() - 1].1;
        Color::Rgb(r, g, b)
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
}

/// Semantic color roles for every panel.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub bg: Color,
    pub panel_bg: Color,
    pub text: Color,
    pub dim: Color,
    pub title: Color,
    pub accent: Color,
    pub border: Color,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    pub selection_bg: Color,
    pub cpu: Gradient,
    pub gpu: Gradient,
    pub power: Gradient,
    pub mem: Gradient,
    pub net_rx: Color,
    pub net_tx: Color,
    /// Thermal-map palette (cool → hot).
    pub thermal: Gradient,
    /// Thermal ramp for 256-color terminals: a hand-curated monotonic walk
    /// through the xterm cube, one channel step at a time. Looking the field
    /// value up here (instead of nearest-quantizing the RGB ramp) keeps the
    /// inevitable banding ordered and fine-grained — clean topographic
    /// contours instead of per-channel rounding noise.
    pub thermal_indexed: &'static [u8],
}

pub const NEON: Theme = Theme {
    name: "neon",
    bg: Color::Rgb(10, 10, 15),
    panel_bg: Color::Rgb(13, 13, 20),
    text: Color::Rgb(230, 230, 240),
    dim: Color::Rgb(105, 105, 130),
    title: Color::Rgb(255, 45, 149),
    accent: Color::Rgb(0, 229, 255),
    border: Color::Rgb(50, 50, 72),
    ok: Color::Rgb(0, 230, 118),
    warn: Color::Rgb(255, 179, 0),
    crit: Color::Rgb(255, 82, 82),
    selection_bg: Color::Rgb(38, 38, 60),
    cpu: Gradient::new(&[
        (0.0, (0, 229, 255)),
        (0.55, (124, 77, 255)),
        (1.0, (255, 45, 149)),
    ]),
    gpu: Gradient::new(&[
        (0.0, (124, 77, 255)),
        (0.6, (0, 190, 255)),
        (1.0, (0, 229, 255)),
    ]),
    power: Gradient::new(&[
        (0.0, (255, 179, 0)),
        (0.6, (255, 120, 60)),
        (1.0, (255, 82, 82)),
    ]),
    mem: Gradient::new(&[
        (0.0, (0, 230, 118)),
        (0.6, (190, 235, 60)),
        (1.0, (255, 234, 0)),
    ]),
    net_rx: Color::Rgb(0, 230, 118),
    net_tx: Color::Rgb(64, 156, 255),
    thermal: Gradient::new(&[
        (0.0, (8, 10, 26)),
        (0.22, (14, 32, 92)),
        (0.42, (0, 105, 190)),
        (0.58, (0, 190, 170)),
        (0.7, (120, 215, 80)),
        (0.8, (245, 215, 60)),
        (0.9, (255, 130, 40)),
        (0.97, (255, 70, 60)),
        (1.0, (255, 240, 235)),
    ]),
    // The truecolor ramp's waypoints walked through the cube's muted
    // levels: near-black → navy → ocean blue → teal → sea green → gold →
    // orange → red → rose. Duplicated entries weight the walk to match the
    // truecolor stop positions (blues span the bottom 40%, rose only the
    // very tip), keeping the dark neon mood — no ff-saturated mids.
    thermal_indexed: &[
        232, 233, 17, 17, 18, 18, 19, 24, 24, 25, 25, 31, 31, 37, 37, 43, 43, 42, 41, 77, 113, 149,
        185, 221, 220, 220, 214, 214, 208, 202, 203, 217,
    ],
};

/// NEON on a true-black canvas: the same electric palette, but the base drops
/// to pure `#000000` (OLED midnight) with panels only a whisper above it. The
/// default theme. Everything below is identical to [`NEON`] except `bg` and
/// `panel_bg` — keep them in sync if the neon accents ever change.
pub const MIDNIGHT: Theme = Theme {
    name: "midnight",
    bg: Color::Rgb(0, 0, 0),
    panel_bg: Color::Rgb(10, 10, 17),
    text: Color::Rgb(230, 230, 240),
    dim: Color::Rgb(105, 105, 130),
    title: Color::Rgb(255, 45, 149),
    accent: Color::Rgb(0, 229, 255),
    border: Color::Rgb(50, 50, 72),
    ok: Color::Rgb(0, 230, 118),
    warn: Color::Rgb(255, 179, 0),
    crit: Color::Rgb(255, 82, 82),
    selection_bg: Color::Rgb(38, 38, 60),
    cpu: Gradient::new(&[
        (0.0, (0, 229, 255)),
        (0.55, (124, 77, 255)),
        (1.0, (255, 45, 149)),
    ]),
    gpu: Gradient::new(&[
        (0.0, (124, 77, 255)),
        (0.6, (0, 190, 255)),
        (1.0, (0, 229, 255)),
    ]),
    power: Gradient::new(&[
        (0.0, (255, 179, 0)),
        (0.6, (255, 120, 60)),
        (1.0, (255, 82, 82)),
    ]),
    mem: Gradient::new(&[
        (0.0, (0, 230, 118)),
        (0.6, (190, 235, 60)),
        (1.0, (255, 234, 0)),
    ]),
    net_rx: Color::Rgb(0, 230, 118),
    net_tx: Color::Rgb(64, 156, 255),
    thermal: Gradient::new(&[
        (0.0, (8, 10, 26)),
        (0.22, (14, 32, 92)),
        (0.42, (0, 105, 190)),
        (0.58, (0, 190, 170)),
        (0.7, (120, 215, 80)),
        (0.8, (245, 215, 60)),
        (0.9, (255, 130, 40)),
        (0.97, (255, 70, 60)),
        (1.0, (255, 240, 235)),
    ]),
    thermal_indexed: &[
        232, 233, 17, 17, 18, 18, 19, 24, 24, 25, 25, 31, 31, 37, 37, 43, 43, 42, 41, 77, 113, 149,
        185, 221, 220, 220, 214, 214, 208, 202, 203, 217,
    ],
};

pub const NORD: Theme = Theme {
    name: "nord",
    bg: Color::Rgb(46, 52, 64),
    panel_bg: Color::Rgb(52, 58, 72),
    text: Color::Rgb(236, 239, 244),
    dim: Color::Rgb(106, 118, 138),
    title: Color::Rgb(136, 192, 208),
    accent: Color::Rgb(129, 161, 193),
    border: Color::Rgb(67, 76, 94),
    ok: Color::Rgb(163, 190, 140),
    warn: Color::Rgb(235, 203, 139),
    crit: Color::Rgb(191, 97, 106),
    selection_bg: Color::Rgb(67, 76, 94),
    cpu: Gradient::new(&[
        (0.0, (136, 192, 208)),
        (0.6, (129, 161, 193)),
        (1.0, (180, 142, 173)),
    ]),
    gpu: Gradient::new(&[(0.0, (180, 142, 173)), (1.0, (136, 192, 208))]),
    power: Gradient::new(&[(0.0, (235, 203, 139)), (1.0, (191, 97, 106))]),
    mem: Gradient::new(&[(0.0, (163, 190, 140)), (1.0, (235, 203, 139))]),
    net_rx: Color::Rgb(163, 190, 140),
    net_tx: Color::Rgb(129, 161, 193),
    thermal: Gradient::new(&[
        (0.0, (46, 52, 64)),
        (0.35, (94, 129, 172)),
        (0.55, (136, 192, 208)),
        (0.7, (235, 203, 139)),
        (0.85, (208, 135, 112)),
        (1.0, (191, 97, 106)),
    ]),
    // Muted aurora walk: slate → frost blues → sand → rust.
    thermal_indexed: &[
        237, 238, 60, 61, 67, 68, 74, 110, 116, 152, 187, 223, 216, 173, 167, 131,
    ],
};

pub const DRACULA: Theme = Theme {
    name: "dracula",
    bg: Color::Rgb(40, 42, 54),
    panel_bg: Color::Rgb(46, 48, 62),
    text: Color::Rgb(248, 248, 242),
    dim: Color::Rgb(98, 114, 164),
    title: Color::Rgb(189, 147, 249),
    accent: Color::Rgb(255, 121, 198),
    border: Color::Rgb(68, 71, 90),
    ok: Color::Rgb(80, 250, 123),
    warn: Color::Rgb(241, 250, 140),
    crit: Color::Rgb(255, 85, 85),
    selection_bg: Color::Rgb(68, 71, 90),
    cpu: Gradient::new(&[
        (0.0, (139, 233, 253)),
        (0.6, (189, 147, 249)),
        (1.0, (255, 121, 198)),
    ]),
    gpu: Gradient::new(&[(0.0, (189, 147, 249)), (1.0, (139, 233, 253))]),
    power: Gradient::new(&[(0.0, (241, 250, 140)), (1.0, (255, 85, 85))]),
    mem: Gradient::new(&[(0.0, (80, 250, 123)), (1.0, (241, 250, 140))]),
    net_rx: Color::Rgb(80, 250, 123),
    net_tx: Color::Rgb(139, 233, 253),
    thermal: Gradient::new(&[
        (0.0, (40, 42, 54)),
        (0.35, (98, 114, 164)),
        (0.55, (139, 233, 253)),
        (0.7, (241, 250, 140)),
        (0.85, (255, 184, 108)),
        (1.0, (255, 85, 85)),
    ]),
    // Night walk: charcoal → comment blues → cyan → yellow → orange → red.
    thermal_indexed: &[
        236, 237, 60, 61, 62, 68, 74, 80, 117, 123, 159, 158, 192, 228, 222, 215, 209, 203,
    ],
};

pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    bg: Color::Rgb(29, 32, 33),
    panel_bg: Color::Rgb(40, 40, 40),
    text: Color::Rgb(235, 219, 178),
    dim: Color::Rgb(146, 131, 116),
    title: Color::Rgb(250, 189, 47),
    accent: Color::Rgb(254, 128, 25),
    border: Color::Rgb(80, 73, 69),
    ok: Color::Rgb(184, 187, 38),
    warn: Color::Rgb(250, 189, 47),
    crit: Color::Rgb(251, 73, 52),
    selection_bg: Color::Rgb(60, 56, 54),
    cpu: Gradient::new(&[
        (0.0, (131, 165, 152)),
        (0.55, (142, 192, 124)),
        (1.0, (250, 189, 47)),
    ]),
    gpu: Gradient::new(&[(0.0, (211, 134, 155)), (1.0, (142, 192, 124))]),
    power: Gradient::new(&[
        (0.0, (250, 189, 47)),
        (0.6, (254, 128, 25)),
        (1.0, (251, 73, 52)),
    ]),
    mem: Gradient::new(&[(0.0, (184, 187, 38)), (1.0, (250, 189, 47))]),
    net_rx: Color::Rgb(184, 187, 38),
    net_tx: Color::Rgb(131, 165, 152),
    thermal: Gradient::new(&[
        (0.0, (29, 32, 33)),
        (0.3, (69, 133, 136)),
        (0.5, (142, 192, 124)),
        (0.68, (215, 153, 33)),
        (0.82, (214, 93, 14)),
        (0.93, (251, 73, 52)),
        (1.0, (251, 236, 214)),
    ]),
    // Retro walk: dark → faded blue → aqua → green → gold → orange → red → cream.
    thermal_indexed: &[
        234, 235, 236, 23, 24, 30, 66, 72, 108, 143, 178, 214, 208, 202, 223,
    ],
};

pub const TOKYONIGHT: Theme = Theme {
    name: "tokyonight",
    bg: Color::Rgb(26, 27, 38),
    panel_bg: Color::Rgb(30, 31, 45),
    text: Color::Rgb(192, 202, 245),
    dim: Color::Rgb(86, 95, 137),
    title: Color::Rgb(122, 162, 247),
    accent: Color::Rgb(187, 154, 247),
    border: Color::Rgb(59, 66, 97),
    ok: Color::Rgb(158, 206, 106),
    warn: Color::Rgb(224, 175, 104),
    crit: Color::Rgb(247, 118, 142),
    selection_bg: Color::Rgb(40, 52, 87),
    cpu: Gradient::new(&[
        (0.0, (125, 207, 255)),
        (0.55, (122, 162, 247)),
        (1.0, (187, 154, 247)),
    ]),
    gpu: Gradient::new(&[(0.0, (187, 154, 247)), (1.0, (125, 207, 255))]),
    power: Gradient::new(&[
        (0.0, (224, 175, 104)),
        (0.6, (255, 158, 100)),
        (1.0, (247, 118, 142)),
    ]),
    mem: Gradient::new(&[
        (0.0, (158, 206, 106)),
        (0.5, (115, 218, 202)),
        (1.0, (224, 175, 104)),
    ]),
    net_rx: Color::Rgb(158, 206, 106),
    net_tx: Color::Rgb(122, 162, 247),
    thermal: Gradient::new(&[
        (0.0, (26, 27, 38)),
        (0.28, (59, 66, 97)),
        (0.44, (122, 162, 247)),
        (0.58, (125, 207, 255)),
        (0.7, (158, 206, 106)),
        (0.8, (224, 175, 104)),
        (0.9, (255, 158, 100)),
        (0.96, (247, 118, 142)),
        (1.0, (255, 235, 238)),
    ]),
    // Night-city walk: ink → slate → blue → cyan → green → amber → coral.
    thermal_indexed: &[
        234, 235, 60, 61, 67, 74, 75, 117, 159, 150, 186, 222, 215, 209, 224,
    ],
};

pub const CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    bg: Color::Rgb(30, 30, 46),
    panel_bg: Color::Rgb(24, 24, 37),
    text: Color::Rgb(205, 214, 244),
    dim: Color::Rgb(108, 112, 134),
    title: Color::Rgb(203, 166, 247),
    accent: Color::Rgb(245, 194, 231),
    border: Color::Rgb(49, 50, 68),
    ok: Color::Rgb(166, 227, 161),
    warn: Color::Rgb(249, 226, 175),
    crit: Color::Rgb(243, 139, 168),
    selection_bg: Color::Rgb(69, 71, 90),
    cpu: Gradient::new(&[
        (0.0, (137, 220, 235)),
        (0.55, (137, 180, 250)),
        (1.0, (203, 166, 247)),
    ]),
    gpu: Gradient::new(&[(0.0, (203, 166, 247)), (1.0, (137, 220, 235))]),
    power: Gradient::new(&[
        (0.0, (250, 179, 135)),
        (0.6, (235, 160, 172)),
        (1.0, (243, 139, 168)),
    ]),
    mem: Gradient::new(&[
        (0.0, (166, 227, 161)),
        (0.5, (148, 226, 213)),
        (1.0, (249, 226, 175)),
    ]),
    net_rx: Color::Rgb(166, 227, 161),
    net_tx: Color::Rgb(137, 180, 250),
    thermal: Gradient::new(&[
        (0.0, (30, 30, 46)),
        (0.26, (69, 71, 90)),
        (0.42, (137, 180, 250)),
        (0.56, (148, 226, 213)),
        (0.68, (166, 227, 161)),
        (0.79, (249, 226, 175)),
        (0.89, (250, 179, 135)),
        (0.96, (243, 139, 168)),
        (1.0, (245, 224, 228)),
    ]),
    // Pastel walk: base → surface → blue → teal → green → yellow → peach → red.
    thermal_indexed: &[
        235, 236, 60, 61, 68, 74, 116, 152, 151, 187, 223, 216, 210, 224,
    ],
};

pub const SOLARIZED: Theme = Theme {
    name: "solarized",
    bg: Color::Rgb(0, 43, 54),
    panel_bg: Color::Rgb(7, 54, 66),
    text: Color::Rgb(131, 148, 150),
    dim: Color::Rgb(88, 110, 117),
    title: Color::Rgb(38, 139, 210),
    accent: Color::Rgb(42, 161, 152),
    border: Color::Rgb(10, 60, 72),
    ok: Color::Rgb(133, 153, 0),
    warn: Color::Rgb(181, 137, 0),
    crit: Color::Rgb(220, 50, 47),
    selection_bg: Color::Rgb(7, 54, 66),
    cpu: Gradient::new(&[
        (0.0, (42, 161, 152)),
        (0.55, (38, 139, 210)),
        (1.0, (108, 113, 196)),
    ]),
    gpu: Gradient::new(&[(0.0, (108, 113, 196)), (1.0, (42, 161, 152))]),
    power: Gradient::new(&[
        (0.0, (181, 137, 0)),
        (0.6, (203, 75, 22)),
        (1.0, (220, 50, 47)),
    ]),
    mem: Gradient::new(&[
        (0.0, (133, 153, 0)),
        (0.5, (42, 161, 152)),
        (1.0, (181, 137, 0)),
    ]),
    net_rx: Color::Rgb(133, 153, 0),
    net_tx: Color::Rgb(38, 139, 210),
    thermal: Gradient::new(&[
        (0.0, (0, 43, 54)),
        (0.28, (88, 110, 117)),
        (0.42, (38, 139, 210)),
        (0.55, (42, 161, 152)),
        (0.68, (133, 153, 0)),
        (0.79, (181, 137, 0)),
        (0.88, (203, 75, 22)),
        (0.96, (220, 50, 47)),
        (1.0, (253, 246, 227)),
    ]),
    // Base03 walk: deep teal → slate → blue → cyan → green → yellow → red → base3.
    thermal_indexed: &[23, 24, 30, 31, 37, 66, 64, 100, 136, 166, 160, 224],
};

pub const ROSEPINE: Theme = Theme {
    name: "rosepine",
    bg: Color::Rgb(25, 23, 36),
    panel_bg: Color::Rgb(31, 29, 46),
    text: Color::Rgb(224, 222, 244),
    dim: Color::Rgb(110, 106, 134),
    title: Color::Rgb(196, 167, 231),
    accent: Color::Rgb(156, 207, 216),
    border: Color::Rgb(64, 61, 82),
    ok: Color::Rgb(156, 207, 216),
    warn: Color::Rgb(246, 193, 119),
    crit: Color::Rgb(235, 111, 146),
    selection_bg: Color::Rgb(64, 61, 82),
    cpu: Gradient::new(&[
        (0.0, (156, 207, 216)),
        (0.55, (196, 167, 231)),
        (1.0, (235, 111, 146)),
    ]),
    gpu: Gradient::new(&[(0.0, (196, 167, 231)), (1.0, (156, 207, 216))]),
    power: Gradient::new(&[
        (0.0, (246, 193, 119)),
        (0.6, (235, 188, 186)),
        (1.0, (235, 111, 146)),
    ]),
    mem: Gradient::new(&[
        (0.0, (156, 207, 216)),
        (0.5, (196, 167, 231)),
        (1.0, (246, 193, 119)),
    ]),
    net_rx: Color::Rgb(156, 207, 216),
    net_tx: Color::Rgb(196, 167, 231),
    thermal: Gradient::new(&[
        (0.0, (25, 23, 36)),
        (0.28, (49, 116, 143)),
        (0.45, (156, 207, 216)),
        (0.6, (196, 167, 231)),
        (0.72, (246, 193, 119)),
        (0.83, (235, 188, 186)),
        (0.93, (235, 111, 146)),
        (1.0, (255, 240, 245)),
    ]),
    // Dawn walk: night → pine → foam → iris → gold → rose → love.
    thermal_indexed: &[234, 235, 23, 66, 73, 116, 146, 183, 223, 217, 211, 225],
};

pub const EVERFOREST: Theme = Theme {
    name: "everforest",
    bg: Color::Rgb(45, 53, 59),
    panel_bg: Color::Rgb(50, 60, 65),
    text: Color::Rgb(211, 198, 170),
    dim: Color::Rgb(133, 146, 137),
    title: Color::Rgb(167, 192, 128),
    accent: Color::Rgb(131, 192, 146),
    border: Color::Rgb(60, 70, 76),
    ok: Color::Rgb(167, 192, 128),
    warn: Color::Rgb(219, 188, 127),
    crit: Color::Rgb(230, 126, 128),
    selection_bg: Color::Rgb(61, 74, 66),
    cpu: Gradient::new(&[
        (0.0, (127, 187, 179)),
        (0.55, (131, 192, 146)),
        (1.0, (167, 192, 128)),
    ]),
    gpu: Gradient::new(&[(0.0, (214, 153, 182)), (1.0, (127, 187, 179))]),
    power: Gradient::new(&[
        (0.0, (219, 188, 127)),
        (0.6, (230, 152, 117)),
        (1.0, (230, 126, 128)),
    ]),
    mem: Gradient::new(&[
        (0.0, (167, 192, 128)),
        (0.5, (131, 192, 146)),
        (1.0, (219, 188, 127)),
    ]),
    net_rx: Color::Rgb(167, 192, 128),
    net_tx: Color::Rgb(127, 187, 179),
    thermal: Gradient::new(&[
        (0.0, (45, 53, 59)),
        (0.28, (127, 187, 179)),
        (0.45, (131, 192, 146)),
        (0.6, (167, 192, 128)),
        (0.72, (219, 188, 127)),
        (0.83, (230, 152, 117)),
        (0.93, (230, 126, 128)),
        (1.0, (240, 232, 210)),
    ]),
    // Forest walk: bark → blue → aqua → leaf → wheat → clay → red.
    thermal_indexed: &[236, 237, 66, 73, 79, 108, 144, 180, 179, 173, 167, 223],
};

pub const KANAGAWA: Theme = Theme {
    name: "kanagawa",
    bg: Color::Rgb(31, 31, 40),
    panel_bg: Color::Rgb(42, 42, 55),
    text: Color::Rgb(220, 215, 186),
    dim: Color::Rgb(114, 113, 105),
    title: Color::Rgb(126, 156, 216),
    accent: Color::Rgb(210, 126, 153),
    border: Color::Rgb(54, 54, 70),
    ok: Color::Rgb(152, 187, 108),
    warn: Color::Rgb(230, 195, 132),
    crit: Color::Rgb(228, 104, 118),
    selection_bg: Color::Rgb(45, 79, 103),
    cpu: Gradient::new(&[
        (0.0, (126, 156, 216)),
        (0.55, (149, 127, 184)),
        (1.0, (210, 126, 153)),
    ]),
    gpu: Gradient::new(&[(0.0, (149, 127, 184)), (1.0, (122, 168, 159))]),
    power: Gradient::new(&[
        (0.0, (230, 195, 132)),
        (0.6, (255, 160, 102)),
        (1.0, (228, 104, 118)),
    ]),
    mem: Gradient::new(&[
        (0.0, (152, 187, 108)),
        (0.5, (122, 168, 159)),
        (1.0, (230, 195, 132)),
    ]),
    net_rx: Color::Rgb(152, 187, 108),
    net_tx: Color::Rgb(126, 156, 216),
    thermal: Gradient::new(&[
        (0.0, (31, 31, 40)),
        (0.26, (45, 79, 103)),
        (0.42, (126, 156, 216)),
        (0.55, (122, 168, 159)),
        (0.68, (152, 187, 108)),
        (0.79, (230, 195, 132)),
        (0.88, (255, 160, 102)),
        (0.96, (228, 104, 118)),
        (1.0, (255, 238, 238)),
    ]),
    // Great-wave walk: sumi → wave-blue → crystal → aqua → spring → carp → red.
    thermal_indexed: &[234, 235, 24, 67, 73, 109, 150, 180, 215, 209, 203, 224],
};

pub const ONEDARK: Theme = Theme {
    name: "onedark",
    bg: Color::Rgb(40, 44, 52),
    panel_bg: Color::Rgb(44, 49, 58),
    text: Color::Rgb(171, 178, 191),
    dim: Color::Rgb(92, 99, 112),
    title: Color::Rgb(97, 175, 239),
    accent: Color::Rgb(198, 120, 221),
    border: Color::Rgb(62, 68, 81),
    ok: Color::Rgb(152, 195, 121),
    warn: Color::Rgb(229, 192, 123),
    crit: Color::Rgb(224, 108, 117),
    selection_bg: Color::Rgb(62, 68, 81),
    cpu: Gradient::new(&[
        (0.0, (86, 182, 194)),
        (0.55, (97, 175, 239)),
        (1.0, (198, 120, 221)),
    ]),
    gpu: Gradient::new(&[(0.0, (198, 120, 221)), (1.0, (86, 182, 194))]),
    power: Gradient::new(&[
        (0.0, (229, 192, 123)),
        (0.6, (209, 154, 102)),
        (1.0, (224, 108, 117)),
    ]),
    mem: Gradient::new(&[
        (0.0, (152, 195, 121)),
        (0.5, (86, 182, 194)),
        (1.0, (229, 192, 123)),
    ]),
    net_rx: Color::Rgb(152, 195, 121),
    net_tx: Color::Rgb(97, 175, 239),
    thermal: Gradient::new(&[
        (0.0, (40, 44, 52)),
        (0.28, (97, 175, 239)),
        (0.45, (86, 182, 194)),
        (0.6, (152, 195, 121)),
        (0.72, (229, 192, 123)),
        (0.83, (209, 154, 102)),
        (0.93, (224, 108, 117)),
        (1.0, (245, 238, 240)),
    ]),
    // Atom walk: gutter → blue → cyan → green → yellow → orange → red.
    thermal_indexed: &[236, 237, 74, 75, 80, 114, 150, 186, 179, 173, 167, 224],
};

pub const SYNTHWAVE: Theme = Theme {
    name: "synthwave",
    bg: Color::Rgb(38, 35, 53),
    panel_bg: Color::Rgb(34, 25, 46),
    text: Color::Rgb(245, 240, 255),
    dim: Color::Rgb(132, 139, 189),
    title: Color::Rgb(255, 126, 219),
    accent: Color::Rgb(54, 249, 246),
    border: Color::Rgb(74, 58, 102),
    ok: Color::Rgb(114, 241, 184),
    warn: Color::Rgb(254, 222, 93),
    crit: Color::Rgb(254, 68, 80),
    selection_bg: Color::Rgb(60, 45, 90),
    cpu: Gradient::new(&[
        (0.0, (54, 249, 246)),
        (0.55, (184, 115, 255)),
        (1.0, (255, 126, 219)),
    ]),
    gpu: Gradient::new(&[(0.0, (184, 115, 255)), (1.0, (54, 249, 246))]),
    power: Gradient::new(&[
        (0.0, (254, 222, 93)),
        (0.6, (255, 140, 80)),
        (1.0, (254, 68, 80)),
    ]),
    mem: Gradient::new(&[
        (0.0, (114, 241, 184)),
        (0.5, (54, 249, 246)),
        (1.0, (254, 222, 93)),
    ]),
    net_rx: Color::Rgb(114, 241, 184),
    net_tx: Color::Rgb(124, 127, 255),
    thermal: Gradient::new(&[
        (0.0, (38, 35, 53)),
        (0.25, (80, 60, 140)),
        (0.4, (54, 249, 246)),
        (0.55, (114, 241, 184)),
        (0.68, (254, 222, 93)),
        (0.8, (255, 140, 80)),
        (0.9, (254, 68, 80)),
        (0.96, (255, 126, 219)),
        (1.0, (255, 245, 255)),
    ]),
    // Outrun walk: dusk → violet → cyan → mint → gold → orange → hot pink.
    thermal_indexed: &[235, 54, 55, 51, 50, 86, 158, 228, 214, 208, 203, 205, 225],
};

pub const MONOKAI: Theme = Theme {
    name: "monokai",
    bg: Color::Rgb(39, 40, 34),
    panel_bg: Color::Rgb(45, 46, 40),
    text: Color::Rgb(248, 248, 242),
    dim: Color::Rgb(117, 113, 94),
    title: Color::Rgb(249, 38, 114),
    accent: Color::Rgb(102, 217, 239),
    border: Color::Rgb(73, 72, 62),
    ok: Color::Rgb(166, 226, 46),
    warn: Color::Rgb(230, 219, 116),
    crit: Color::Rgb(249, 38, 114),
    selection_bg: Color::Rgb(73, 72, 62),
    cpu: Gradient::new(&[
        (0.0, (102, 217, 239)),
        (0.55, (174, 129, 255)),
        (1.0, (249, 38, 114)),
    ]),
    gpu: Gradient::new(&[(0.0, (174, 129, 255)), (1.0, (102, 217, 239))]),
    power: Gradient::new(&[
        (0.0, (230, 219, 116)),
        (0.6, (253, 151, 31)),
        (1.0, (249, 38, 114)),
    ]),
    mem: Gradient::new(&[
        (0.0, (166, 226, 46)),
        (0.5, (102, 217, 239)),
        (1.0, (230, 219, 116)),
    ]),
    net_rx: Color::Rgb(166, 226, 46),
    net_tx: Color::Rgb(102, 217, 239),
    thermal: Gradient::new(&[
        (0.0, (39, 40, 34)),
        (0.26, (102, 217, 239)),
        (0.42, (166, 226, 46)),
        (0.6, (230, 219, 116)),
        (0.75, (253, 151, 31)),
        (0.9, (249, 38, 114)),
        (1.0, (255, 245, 250)),
    ]),
    // Vivid walk: olive-black → cyan → lime → yellow → orange → magenta.
    thermal_indexed: &[235, 236, 80, 81, 155, 191, 228, 214, 208, 197, 225],
};

pub const CYBERPUNK: Theme = Theme {
    name: "cyberpunk",
    bg: Color::Rgb(10, 11, 20),
    panel_bg: Color::Rgb(16, 17, 28),
    text: Color::Rgb(240, 240, 245),
    dim: Color::Rgb(90, 100, 120),
    title: Color::Rgb(252, 238, 10),
    accent: Color::Rgb(0, 240, 255),
    border: Color::Rgb(40, 44, 66),
    ok: Color::Rgb(0, 255, 159),
    warn: Color::Rgb(252, 238, 10),
    crit: Color::Rgb(255, 0, 60),
    selection_bg: Color::Rgb(34, 38, 60),
    cpu: Gradient::new(&[
        (0.0, (0, 240, 255)),
        (0.5, (214, 0, 255)),
        (1.0, (255, 0, 60)),
    ]),
    gpu: Gradient::new(&[(0.0, (214, 0, 255)), (1.0, (0, 240, 255))]),
    power: Gradient::new(&[
        (0.0, (252, 238, 10)),
        (0.6, (255, 138, 0)),
        (1.0, (255, 0, 60)),
    ]),
    mem: Gradient::new(&[
        (0.0, (0, 255, 159)),
        (0.5, (0, 240, 255)),
        (1.0, (252, 238, 10)),
    ]),
    net_rx: Color::Rgb(0, 255, 159),
    net_tx: Color::Rgb(0, 160, 255),
    thermal: Gradient::new(&[
        (0.0, (10, 11, 20)),
        (0.22, (0, 80, 160)),
        (0.4, (0, 240, 255)),
        (0.55, (0, 255, 159)),
        (0.68, (252, 238, 10)),
        (0.8, (255, 138, 0)),
        (0.9, (255, 0, 60)),
        (0.96, (255, 0, 160)),
        (1.0, (255, 245, 250)),
    ]),
    // Night-city walk: black → blue → cyan → green → yellow → orange → red → magenta.
    thermal_indexed: &[232, 17, 25, 39, 51, 48, 226, 220, 208, 196, 199, 225],
};

pub const SOLARIZED_LIGHT: Theme = Theme {
    name: "solarized-light",
    bg: Color::Rgb(253, 246, 227),
    panel_bg: Color::Rgb(238, 232, 213),
    text: Color::Rgb(88, 110, 117),
    dim: Color::Rgb(147, 161, 161),
    title: Color::Rgb(38, 139, 210),
    accent: Color::Rgb(211, 54, 130),
    border: Color::Rgb(147, 161, 161),
    ok: Color::Rgb(133, 153, 0),
    warn: Color::Rgb(181, 137, 0),
    crit: Color::Rgb(220, 50, 47),
    selection_bg: Color::Rgb(221, 214, 188),
    cpu: Gradient::new(&[
        (0.0, (42, 161, 152)),
        (0.55, (38, 139, 210)),
        (1.0, (108, 113, 196)),
    ]),
    gpu: Gradient::new(&[(0.0, (108, 113, 196)), (1.0, (42, 161, 152))]),
    power: Gradient::new(&[
        (0.0, (181, 137, 0)),
        (0.6, (203, 75, 22)),
        (1.0, (220, 50, 47)),
    ]),
    mem: Gradient::new(&[
        (0.0, (133, 153, 0)),
        (0.5, (42, 161, 152)),
        (1.0, (181, 137, 0)),
    ]),
    net_rx: Color::Rgb(133, 153, 0),
    net_tx: Color::Rgb(38, 139, 210),
    // Light theme: cool floor near bg, hot end stays deep (not white-hot, which
    // would vanish on a light background). temp_color samples from t≥0.35, so
    // everything from the blue stop up is dark enough to read as text.
    thermal: Gradient::new(&[
        (0.0, (238, 232, 213)),
        (0.2, (147, 161, 161)),
        (0.38, (38, 139, 210)),
        (0.55, (42, 161, 152)),
        (0.68, (133, 153, 0)),
        (0.79, (181, 137, 0)),
        (0.88, (203, 75, 22)),
        (1.0, (220, 50, 47)),
    ]),
    // Light walk: parchment → base1 → blue → cyan → green → yellow → orange → red.
    thermal_indexed: &[223, 187, 145, 38, 37, 72, 100, 136, 166, 160],
};

pub const LATTE: Theme = Theme {
    name: "latte",
    bg: Color::Rgb(239, 241, 245),
    panel_bg: Color::Rgb(230, 233, 239),
    text: Color::Rgb(76, 79, 105),
    dim: Color::Rgb(108, 111, 133),
    title: Color::Rgb(136, 57, 239),
    accent: Color::Rgb(23, 146, 153),
    border: Color::Rgb(188, 192, 204),
    ok: Color::Rgb(64, 160, 43),
    warn: Color::Rgb(223, 142, 29),
    crit: Color::Rgb(210, 15, 57),
    selection_bg: Color::Rgb(204, 208, 218),
    cpu: Gradient::new(&[
        (0.0, (4, 165, 229)),
        (0.55, (30, 102, 245)),
        (1.0, (136, 57, 239)),
    ]),
    gpu: Gradient::new(&[(0.0, (136, 57, 239)), (1.0, (4, 165, 229))]),
    power: Gradient::new(&[
        (0.0, (223, 142, 29)),
        (0.6, (254, 100, 11)),
        (1.0, (210, 15, 57)),
    ]),
    mem: Gradient::new(&[
        (0.0, (64, 160, 43)),
        (0.5, (23, 146, 153)),
        (1.0, (223, 142, 29)),
    ]),
    net_rx: Color::Rgb(64, 160, 43),
    net_tx: Color::Rgb(30, 102, 245),
    // Light theme: hot end stays deep so it reads on the light base (see note in
    // SOLARIZED_LIGHT). temp_color floors at t≥0.35 → blue upward is inky enough.
    thermal: Gradient::new(&[
        (0.0, (230, 233, 239)),
        (0.2, (156, 160, 176)),
        (0.38, (30, 102, 245)),
        (0.55, (23, 146, 153)),
        (0.68, (64, 160, 43)),
        (0.79, (223, 142, 29)),
        (0.88, (254, 100, 11)),
        (1.0, (210, 15, 57)),
    ]),
    // Light walk: mist → overlay → blue → teal → green → yellow → peach → red.
    thermal_indexed: &[251, 145, 111, 33, 37, 72, 64, 178, 172, 160],
};

pub const GRUVBOX_LIGHT: Theme = Theme {
    name: "gruvbox-light",
    bg: Color::Rgb(249, 245, 215),
    panel_bg: Color::Rgb(235, 219, 178),
    text: Color::Rgb(60, 56, 54),
    dim: Color::Rgb(146, 131, 116),
    title: Color::Rgb(181, 118, 20),
    accent: Color::Rgb(66, 123, 88),
    border: Color::Rgb(189, 174, 147),
    ok: Color::Rgb(121, 116, 14),
    warn: Color::Rgb(181, 118, 20),
    crit: Color::Rgb(157, 0, 6),
    selection_bg: Color::Rgb(213, 196, 161),
    cpu: Gradient::new(&[
        (0.0, (7, 102, 120)),
        (0.55, (66, 123, 88)),
        (1.0, (181, 118, 20)),
    ]),
    gpu: Gradient::new(&[(0.0, (143, 63, 113)), (1.0, (7, 102, 120))]),
    power: Gradient::new(&[
        (0.0, (181, 118, 20)),
        (0.6, (175, 58, 3)),
        (1.0, (157, 0, 6)),
    ]),
    mem: Gradient::new(&[
        (0.0, (121, 116, 14)),
        (0.5, (66, 123, 88)),
        (1.0, (181, 118, 20)),
    ]),
    net_rx: Color::Rgb(121, 116, 14),
    net_tx: Color::Rgb(7, 102, 120),
    // Light theme: hot end stays deep red so it reads on the light base (see note
    // in SOLARIZED_LIGHT). temp_color floors at t≥0.35 → blue upward is inky.
    thermal: Gradient::new(&[
        (0.0, (235, 219, 178)),
        (0.2, (168, 153, 132)),
        (0.38, (7, 102, 120)),
        (0.55, (66, 123, 88)),
        (0.68, (121, 116, 14)),
        (0.79, (181, 118, 20)),
        (0.88, (175, 58, 3)),
        (1.0, (157, 0, 6)),
    ]),
    // Light walk: cream → taupe → blue → aqua → green → gold → orange → red.
    thermal_indexed: &[187, 144, 23, 24, 30, 64, 100, 136, 130, 124],
};

/// Every registered theme, in cycle order (`t` key). Dark themes first, the
/// three light themes last so cycling doesn't jump to a light background
/// mid-run. A slice, not a fixed array — new themes append without a length.
/// `MIDNIGHT` leads as the default.
pub const THEMES: &[Theme] = &[
    MIDNIGHT,
    NEON,
    NORD,
    DRACULA,
    GRUVBOX,
    TOKYONIGHT,
    CATPPUCCIN,
    SOLARIZED,
    ROSEPINE,
    EVERFOREST,
    KANAGAWA,
    ONEDARK,
    SYNTHWAVE,
    MONOKAI,
    CYBERPUNK,
    SOLARIZED_LIGHT,
    LATTE,
    GRUVBOX_LIGHT,
];

pub fn by_name(name: &str) -> Theme {
    THEMES
        .iter()
        .copied()
        .find(|t| t.name == name)
        .unwrap_or(MIDNIGHT)
}

/// Absolute temperature → thermal-ramp position: 25 °C ambient → 110 °C
/// throttle ceiling. The single mapping every thermal color shares (the
/// isotherm map's rings and fills, temperature text everywhere), so a
/// given °C always lands on the same hue.
pub fn temp_ratio(celsius: f32) -> f32 {
    ((celsius - 25.0) / 85.0).clamp(0.0, 1.0)
}

impl Theme {
    /// Color for a temperature shown as *text* (floored away from the ramp's
    /// near-black cold end so cool values stay readable).
    pub fn temp_color(&self, celsius: f32) -> Color {
        self.thermal.at(temp_ratio(celsius).max(0.35))
    }

    /// Severity color for a 0..=1 utilization-style ratio.
    pub fn severity(&self, ratio: f32) -> Color {
        if ratio >= 0.9 {
            self.crit
        } else if ratio >= 0.7 {
            self.warn
        } else {
            self.ok
        }
    }
}
