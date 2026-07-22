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
    /// Cycles the pack is rated for (`DesignCycleCount9C`), which turns the
    /// bare cycle count into a fraction of design life.
    pub design_cycles: Option<u32>,
    /// Per-cell voltages in mV. Cells drifting apart is the earliest visible
    /// sign of a pack going bad.
    pub cell_voltages: Vec<u32>,
    /// Why the pack is not charging despite being on AC — 0 means no reason,
    /// i.e. it either is charging or is simply full.
    pub not_charging_reason: Option<u64>,
    /// Seconds of charging the pack has spent thermally limited.
    pub thermally_limited_secs: Option<u64>,
    /// The optimized-charging band the pack has been living in, as
    /// `(min, max)` percent over recent days.
    pub daily_soc: Option<(u32, u32)>,
    /// Peak pack temperature ever recorded, in °C.
    pub lifetime_max_temp: Option<Celsius>,
    /// Real charge in mAh, alongside the percentage.
    pub raw_capacity_mah: Option<u32>,
    pub raw_max_capacity_mah: Option<u32>,
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
    design_cycles: CfOwned,
    battery_data: CfOwned,
    cell_voltage: CfOwned,
    daily_min_soc: CfOwned,
    daily_max_soc: CfOwned,
    lifetime_data: CfOwned,
    max_temperature: CfOwned,
    charger_data: CfOwned,
    not_charging_reason: CfOwned,
    thermally_limited: CfOwned,
    raw_capacity: CfOwned,
    raw_max_capacity: CfOwned,
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
                design_cycles: cfstr("DesignCycleCount9C"),
                battery_data: cfstr("BatteryData"),
                cell_voltage: cfstr("CellVoltage"),
                daily_min_soc: cfstr("DailyMinSoc"),
                daily_max_soc: cfstr("DailyMaxSoc"),
                lifetime_data: cfstr("LifetimeData"),
                max_temperature: cfstr("MaximumTemperature"),
                charger_data: cfstr("ChargerData"),
                not_charging_reason: cfstr("NotChargingReason"),
                thermally_limited: cfstr("TimeChargingThermallyLimited"),
                raw_capacity: cfstr("AppleRawCurrentCapacity"),
                raw_max_capacity: cfstr("AppleRawMaxCapacity"),
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

        // Depth lives in nested sub-dictionaries of the same property table —
        // no extra registry traffic, just more of what was already copied.
        let sub = |parent: &CfOwned, key: &CfOwned| -> Option<i64> {
            let d: CFDictionaryRef = dict_get(dict, parent)?.cast();
            dict_get(d, key).and_then(cf_number_i64)
        };
        let cell_voltages = dict_get(dict, &k.battery_data)
            .and_then(|p| {
                let bd: CFDictionaryRef = p.cast();
                dict_get(bd, &k.cell_voltage)
            })
            .map(|arr| {
                crate::ffi::cf::array_iter(arr.cast())
                    .filter_map(cf_number_i64)
                    .map(|mv| mv.clamp(0, i64::from(u32::MAX)) as u32)
                    .collect()
            })
            .unwrap_or_default();
        let daily_soc = sub(&k.battery_data, &k.daily_min_soc)
            .zip(sub(&k.battery_data, &k.daily_max_soc))
            .map(|(lo, hi)| (lo.max(0) as u32, hi.max(0) as u32));
        // LifetimeData nests one level deeper than the others.
        let lifetime_max_temp = dict_get(dict, &k.battery_data)
            .and_then(|p| {
                let bd: CFDictionaryRef = p.cast();
                dict_get(bd, &k.lifetime_data)
            })
            .and_then(|p| {
                let ld: CFDictionaryRef = p.cast();
                dict_get(ld, &k.max_temperature).and_then(cf_number_i64)
            })
            .map(|t| Celsius(t as f32));

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
            design_cycles: num(&k.design_cycles).and_then(|c| (c > 0).then_some(c as u32)),
            cell_voltages,
            not_charging_reason: sub(&k.charger_data, &k.not_charging_reason).map(|r| r as u64),
            thermally_limited_secs: sub(&k.charger_data, &k.thermally_limited)
                .map(|s| s.max(0) as u64),
            daily_soc,
            lifetime_max_temp,
            raw_capacity_mah: num(&k.raw_capacity).map(|c| c.max(0) as u32),
            raw_max_capacity_mah: num(&k.raw_max_capacity).map(|c| c.max(0) as u32),
        })
    }
}

/// Whether a pack's cells have drifted apart enough to be worth flagging.
///
/// A healthy pack holds its cells within a few mV; sustained imbalance is the
/// earliest visible sign of one going bad. `None` when there is nothing to
/// compare — a single cell cannot be out of balance with itself.
pub fn cell_imbalance_mv(cells: &[u32]) -> Option<u32> {
    (cells.len() > 1).then(|| {
        let hi = cells.iter().copied().max().unwrap_or(0);
        let lo = cells.iter().copied().min().unwrap_or(0);
        hi.saturating_sub(lo)
    })
}

#[cfg(test)]
mod tests {
    use super::cell_imbalance_mv;

    #[test]
    fn imbalance_is_the_spread_across_cells() {
        assert_eq!(cell_imbalance_mv(&[4096, 4096, 4096]), Some(0));
        assert_eq!(cell_imbalance_mv(&[4090, 4096, 4101]), Some(11));
    }

    #[test]
    fn imbalance_needs_at_least_two_cells() {
        // One cell (or none) is not "perfectly balanced" — it is unknown, and
        // must not render as a confident 0 mV.
        assert_eq!(cell_imbalance_mv(&[]), None);
        assert_eq!(cell_imbalance_mv(&[4096]), None);
    }
}
