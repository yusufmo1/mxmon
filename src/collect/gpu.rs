//! GPU device utilization from the `AGXAccelerator` performance statistics
//! (the same numbers Activity Monitor's GPU meter shows).

use std::io;

use crate::ffi::cf::{CfOwned, cfstr, dict_get};
use crate::ffi::iokit::{IoObject, cf_number_i64, services};
use crate::units::{Bytes, Ratio};

#[derive(Debug, Clone, Default)]
pub struct GpuSample {
    pub device: Ratio,
    pub renderer: Ratio,
    pub tiler: Ratio,
    pub used_memory: Bytes,
}

pub struct GpuCollector {
    accelerator: IoObject,
    perf_key: CfOwned,
    device_key: CfOwned,
    renderer_key: CfOwned,
    tiler_key: CfOwned,
    mem_key: CfOwned,
}

impl GpuCollector {
    pub fn new() -> io::Result<Self> {
        let accelerator = services("AGXAccelerator")?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("AGXAccelerator not found"))?;
        Ok(Self {
            accelerator,
            perf_key: cfstr("PerformanceStatistics"),
            device_key: cfstr("Device Utilization %"),
            renderer_key: cfstr("Renderer Utilization %"),
            tiler_key: cfstr("Tiler Utilization %"),
            mem_key: cfstr("In use system memory"),
        })
    }

    pub fn sample(&self) -> io::Result<GpuSample> {
        // Copy just the PerformanceStatistics sub-dictionary; the full AGX
        // property table is enormous and this runs every fast tick.
        let props = self
            .accelerator
            .property(&self.perf_key)
            .ok_or_else(|| io::Error::other("no PerformanceStatistics"))?;
        let stats: core_foundation::dictionary::CFDictionaryRef = props.as_dict();
        let pct = |key| {
            dict_get(stats, key)
                .and_then(cf_number_i64)
                .map(|v| Ratio(v as f32 / 100.0).clamped())
                .unwrap_or_default()
        };
        Ok(GpuSample {
            device: pct(&self.device_key),
            renderer: pct(&self.renderer_key),
            tiler: pct(&self.tiler_key),
            used_memory: Bytes(
                dict_get(stats, &self.mem_key)
                    .and_then(cf_number_i64)
                    .unwrap_or(0) as u64,
            ),
        })
    }
}
