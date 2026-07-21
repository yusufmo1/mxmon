//! Battery and power-adapter state from the `AppleSmartBattery` registry
//! entry. Desktop Macs simply have no such service → collector reports None.

use core_foundation::dictionary::CFDictionaryRef;

use crate::ffi::cf::{CfOwned, cfstr, dict_get};
use crate::ffi::iokit::{cf_number_i64, services};
use crate::units::{Celsius, Ratio, Watts};

#[derive(Debug, Clone, Default)]
pub struct BatterySample {
    /// Charge level 0..=1.
    pub charge: Ratio,
    pub charging: bool,
    pub external_power: bool,
    pub fully_charged: bool,
    /// Signed: positive while charging, negative while discharging.
    pub battery_watts: Watts,
    /// Wattage of the connected adapter, if any.
    pub adapter_watts: Option<Watts>,
    pub adapter_name: Option<String>,
    pub cycle_count: u32,
    /// Current full-charge capacity vs design capacity.
    pub health: Ratio,
    pub temp: Celsius,
    /// Minutes to full (charging) or empty (discharging), when the OS knows.
    pub minutes_remaining: Option<u32>,
}

pub struct BatteryCollector {
    keys: Keys,
}

struct Keys {
    current_capacity: CfOwned,
    is_charging: CfOwned,
    external: CfOwned,
    fully_charged: CfOwned,
    amperage: CfOwned,
    voltage: CfOwned,
    cycle_count: CfOwned,
    design_capacity: CfOwned,
    nominal_capacity: CfOwned,
    temperature: CfOwned,
    time_remaining: CfOwned,
    adapter_details: CfOwned,
    adapter_watts: CfOwned,
    adapter_name: CfOwned,
}

impl BatteryCollector {
    pub fn new() -> Self {
        Self {
            keys: Keys {
                current_capacity: cfstr("CurrentCapacity"),
                is_charging: cfstr("IsCharging"),
                external: cfstr("ExternalConnected"),
                fully_charged: cfstr("FullyCharged"),
                amperage: cfstr("Amperage"),
                voltage: cfstr("Voltage"),
                cycle_count: cfstr("CycleCount"),
                design_capacity: cfstr("DesignCapacity"),
                nominal_capacity: cfstr("NominalChargeCapacity"),
                temperature: cfstr("Temperature"),
                time_remaining: cfstr("TimeRemaining"),
                adapter_details: cfstr("AdapterDetails"),
                adapter_watts: cfstr("Watts"),
                adapter_name: cfstr("Name"),
            },
        }
    }

    /// `None` when the machine has no battery (or the service is unreadable).
    pub fn sample(&self) -> Option<BatterySample> {
        let battery = services("AppleSmartBattery").ok()?.into_iter().next()?;
        let props = battery.properties().ok()?;
        let dict = props.as_dict();
        let k = &self.keys;

        let num = |key: &CfOwned| dict_get(dict, key).and_then(cf_number_i64);
        let boolean = |key: &CfOwned| dict_get(dict, key).is_some_and(crate::ffi::cf::bool_from_cf);

        let charge_pct = num(&k.current_capacity)?; // 0–100 on Apple Silicon
        let amperage_ma = num(&k.amperage).unwrap_or(0); // signed mA
        let voltage_mv = num(&k.voltage).unwrap_or(0);
        let battery_watts = Watts((amperage_ma as f32 / 1000.0) * (voltage_mv as f32 / 1000.0));

        let design = num(&k.design_capacity).unwrap_or(0);
        let nominal = num(&k.nominal_capacity).unwrap_or(0);
        let health = if design > 0 {
            Ratio(nominal as f32 / design as f32)
        } else {
            Ratio(0.0)
        };

        let adapter: Option<(Option<Watts>, Option<String>)> = dict_get(dict, &k.adapter_details)
            .map(|p| {
                let ad: CFDictionaryRef = p.cast();
                let watts = dict_get(ad, &k.adapter_watts).and_then(cf_number_i64);
                let name = dict_get(ad, &k.adapter_name)
                    .map(|s| crate::ffi::cf::string_from_cf(s.cast()))
                    .filter(|s| !s.is_empty());
                (watts.filter(|&w| w > 0).map(|w| Watts(w as f32)), name)
            });
        let (adapter_watts, adapter_name) = adapter.unwrap_or((None, None));

        let minutes = num(&k.time_remaining).and_then(|m| {
            // 65535 = "calculating"; ignore.
            (m > 0 && m < 60_000).then_some(m as u32)
        });

        Some(BatterySample {
            charge: Ratio(charge_pct as f32 / 100.0).clamped(),
            charging: boolean(&k.is_charging),
            external_power: boolean(&k.external),
            fully_charged: boolean(&k.fully_charged),
            battery_watts,
            adapter_watts,
            adapter_name,
            cycle_count: num(&k.cycle_count).unwrap_or(0) as u32,
            health,
            temp: Celsius(num(&k.temperature).unwrap_or(0) as f32 / 100.0),
            minutes_remaining: minutes,
        })
    }
}
