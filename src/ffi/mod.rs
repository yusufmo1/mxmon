//! All `unsafe` FFI lives below this module; everything exported upward is a
//! safe, owned Rust API. No raw pointers, CF refs, or kern_return codes leak.

// `unsafe_code` is denied crate-wide (Cargo.toml [lints]); this subtree is the
// one sanctioned exception.
#![allow(unsafe_code)]

pub mod cf;
pub mod hid;
pub mod icmp;
pub mod iokit;
pub mod iopm;
pub mod ioreport;
pub mod mach;
pub mod net;
pub mod notify;
pub mod ntstat;
pub mod nvme;
pub mod proc;
pub mod smc;
pub mod sys;
