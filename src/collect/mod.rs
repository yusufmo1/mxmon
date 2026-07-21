//! Metric collectors. Each is independent and degrades gracefully: a failing
//! source disables its panel, never the app.

pub mod battery;
pub mod cpu;
pub mod disk;
pub mod flows;
pub mod gpu;
pub mod mem;
pub mod net;
pub mod ping;
pub mod power;
pub mod procs;
pub mod sampler;
pub mod selfcpu;
pub mod soc;
pub mod temps;
