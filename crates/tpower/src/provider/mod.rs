use std::{
    collections::VecDeque,
    ffi::CString,
    mem,
    ops::{Deref, Div},
    time::Duration,
};

use anyhow::bail;
use core_foundation::{
    base::{kCFAllocatorDefault, TCFType},
    dictionary::{CFDictionary, CFMutableDictionaryRef},
};
use derive_more::Add;
use io_kit_sys::{
    kIOMasterPortDefault, ret::kIOReturnSuccess, IOObjectRelease,
    IORegistryEntryCreateCFProperties, IOServiceGetMatchingService, IOServiceMatching,
};
use ratatui::widgets::SparklineBar;
use serde::{Deserialize, Serialize};

use crate::{
    de::{repr, IORegistry},
    ffi::{smc::SMCPowerData, InterfaceType},
    util::{dict_into, skip_until},
};

#[cfg(feature = "ios-monitoring")]
pub mod remote;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct NormalizedResource {
    pub is_local: bool,
    pub is_charging: bool,
    pub time_remain: Duration,
    pub last_update: i64,
    pub adapter_name: Option<String>,
    pub cycle_count: i32,
    pub current_capacity: i32,
    pub max_capacity: i32,
    #[serde(default)]
    pub design_capacity: i32,
    #[serde(default)]
    pub brightness_power_available: bool,
    #[serde(default)]
    pub heatpipe_power_available: bool,
    #[serde(flatten)]
    pub data: NormalizedData,
}

#[derive(Debug, Clone, Copy, Default, Add, Deserialize, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct NormalizedData {
    pub system_in: f32,
    pub system_load: f32,
    pub battery_power: f32,
    pub adapter_power: f32,
    pub efficiency_loss: f32,
    /// 0 if not available
    pub brightness_power: f32,
    /// 0 if not available
    pub heatpipe_power: f32,
    pub battery_level: i32,
    pub absolute_battery_level: f32,
    pub temperature: f32,

    pub adapter_watts: f32,
    pub adapter_voltage: f32,
    pub adapter_amperage: f32,
}

impl NormalizedData {
    pub fn max_with(self, other: &Self) -> Self {
        Self {
            system_in: self.system_in.max(other.system_in),
            system_load: self.system_load.max(other.system_load),
            battery_power: self.battery_power.max(other.battery_power),
            adapter_power: self.adapter_power.max(other.adapter_power),
            efficiency_loss: self.efficiency_loss.max(other.efficiency_loss),
            battery_level: self.battery_level.max(other.battery_level),
            absolute_battery_level: self
                .absolute_battery_level
                .max(other.absolute_battery_level),
            temperature: self.temperature.max(other.temperature),
            brightness_power: self.brightness_power.max(other.brightness_power),
            heatpipe_power: self.heatpipe_power.max(other.heatpipe_power),
            adapter_watts: self.adapter_watts.max(other.adapter_watts),
            adapter_voltage: self.adapter_voltage.max(other.adapter_voltage),
            adapter_amperage: self.adapter_amperage.max(other.adapter_amperage),
        }
    }
}

impl Div<f32> for NormalizedData {
    type Output = Self;

    fn div(self, rhs: f32) -> Self::Output {
        Self {
            system_in: self.system_in / rhs,
            system_load: self.system_load / rhs,
            battery_power: self.battery_power / rhs,
            adapter_power: self.adapter_power / rhs,
            efficiency_loss: self.efficiency_loss / rhs,
            brightness_power: self.brightness_power / rhs,
            heatpipe_power: self.heatpipe_power / rhs,
            battery_level: self.battery_level / rhs as i32,
            absolute_battery_level: self.absolute_battery_level / rhs,
            temperature: self.temperature / rhs,
            adapter_watts: self.adapter_watts / rhs,
            adapter_voltage: self.adapter_voltage / rhs,
            adapter_amperage: self.adapter_amperage / rhs,
        }
    }
}

impl Deref for NormalizedResource {
    type Target = NormalizedData;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// Battery charge as a percentage of raw max capacity. Returns 0.0 when either
/// raw value is missing or max is non-positive, avoiding a 0/0 -> NaN or x/0 ->
/// inf in the emitted telemetry.
fn absolute_battery_level(io: &IORegistry) -> f32 {
    match (io.apple_raw_current_capacity, io.apple_raw_max_capacity) {
        (Some(current), Some(max)) if max > 0 => current as f32 / max as f32 * 100.,
        _ => 0.,
    }
}

/// Convert a battery estimate to a duration while rejecting invalid sensor
/// values and macOS sentinel values such as 65535 minutes.
pub fn duration_from_minutes(minutes: f32) -> Duration {
    const MAX_REASONABLE_MINUTES: f32 = 24.0 * 60.0;

    if minutes.is_finite() && (0.0..=MAX_REASONABLE_MINUTES).contains(&minutes) {
        Duration::from_secs_f32(minutes * 60.0)
    } else {
        Duration::ZERO
    }
}

impl From<&IORegistry> for NormalizedResource {
    fn from(io: &IORegistry) -> Self {
        let (system_in, system_load, battery_power, adapter_power, efficiency_loss) =
            if let Some(d) = io.ptd() {
                (
                    d.system_power_in as f32 / 1000.,
                    d.system_load as f32 / 1000.,
                    d.battery_power as f32 / 1000.,
                    (d.system_power_in + d.adapter_efficiency_loss) as f32 / 1000.,
                    d.adapter_efficiency_loss as f32 / 1000.,
                )
            } else {
                Default::default()
            };

        Self {
            is_local: false,
            is_charging: io.is_charging.unwrap_or_default(),
            time_remain: duration_from_minutes(io.time_remaining.unwrap_or_default() as f32),
            last_update: io.update_time.unwrap_or_default(),
            adapter_name: io
                .adapter_details
                .name
                .clone()
                .or_else(|| io.adapter_details.description.clone()),
            cycle_count: io.cycle_count.unwrap_or_default(),
            max_capacity: io.apple_raw_max_capacity.unwrap_or_default(),
            design_capacity: io.design_capacity.unwrap_or_default(),
            current_capacity: io.apple_raw_current_capacity.unwrap_or_default(),
            brightness_power_available: false,
            heatpipe_power_available: false,
            data: NormalizedData {
                system_in,
                system_load,
                battery_power,
                adapter_power,
                efficiency_loss,
                brightness_power: 0.,
                heatpipe_power: 0.,
                battery_level: io.current_capacity.unwrap_or_default(),
                absolute_battery_level: absolute_battery_level(io),
                temperature: io.temperature.unwrap_or_default() as f32 / 100.,

                adapter_watts: io.adapter_details.watts.unwrap_or_default() as f32,
                adapter_voltage: io.adapter_details.adapter_voltage.unwrap_or_default() as f32
                    / 1000.,
                adapter_amperage: io.adapter_details.current.unwrap_or_default() as f32 / 1000.,
            },
        }
    }
}

impl From<(&IORegistry, &SMCPowerData)> for NormalizedResource {
    fn from((io, smc): (&IORegistry, &SMCPowerData)) -> Self {
        Self {
            is_local: true,
            last_update: io.update_time.unwrap_or_default(),
            is_charging: smc.is_charging(),
            time_remain: duration_from_minutes(if smc.is_charging() {
                smc.time_to_full
            } else {
                smc.time_to_empty
            }),
            adapter_name: io
                .adapter_details
                .name
                .clone()
                .or_else(|| io.adapter_details.description.clone()),
            cycle_count: io.cycle_count.unwrap_or_default(),
            max_capacity: io.apple_raw_max_capacity.unwrap_or_default(),
            design_capacity: io.design_capacity.unwrap_or_default(),
            current_capacity: io.apple_raw_current_capacity.unwrap_or_default(),
            brightness_power_available: smc.brightness_available,
            heatpipe_power_available: smc.heatpipe_available,
            data: NormalizedData {
                system_in: smc.delivery_rate,
                system_load: smc.system_total,
                battery_power: smc.battery_rate.max(smc.delivery_rate - smc.system_total),
                efficiency_loss: io
                    .ptd()
                    .map_or(0.0, |d| d.adapter_efficiency_loss as f32 / 1000.),
                brightness_power: smc.brightness,
                heatpipe_power: smc.heatpipe,
                battery_level: io.current_capacity.unwrap_or_default(),
                absolute_battery_level: absolute_battery_level(io),
                temperature: smc.temperature,
                adapter_power: smc.delivery_rate
                    + io.ptd()
                        .map_or(0.0, |d| d.adapter_efficiency_loss as f32 / 1000.),

                adapter_watts: io.adapter_details.watts.unwrap_or_default() as f32,
                adapter_voltage: io.adapter_details.adapter_voltage.unwrap_or_default() as f32
                    / 1000.,
                adapter_amperage: io.adapter_details.current.unwrap_or_default() as f32 / 1000.,
            },
        }
    }
}

pub fn get_mac_ioreg_dict() -> anyhow::Result<CFDictionary> {
    let name = CString::new("AppleSmartBattery").unwrap();
    let matching_dict = unsafe { IOServiceMatching(name.as_ptr()) };

    let service = unsafe { IOServiceGetMatchingService(kIOMasterPortDefault, matching_dict) };
    if service == 0 {
        bail!("AppleSmartBattery service not found");
    }

    let mut properties: CFMutableDictionaryRef = unsafe { mem::zeroed() };
    let status = unsafe {
        IORegistryEntryCreateCFProperties(service, &mut properties, kCFAllocatorDefault, 0)
    };
    unsafe { IOObjectRelease(service) };

    if status != kIOReturnSuccess {
        bail!("could not get AppleSmartBattery properties (status={status})");
    }
    if properties.is_null() {
        bail!("AppleSmartBattery returned no properties");
    }

    unsafe { Ok(CFDictionary::wrap_under_create_rule(properties)) }
}

#[cfg(test)]
mod tests {
    use super::{duration_from_minutes, NormalizedResource};
    use crate::{de::IORegistry, ffi::smc::SMCPowerData};
    use std::time::Duration;

    #[test]
    fn rejects_invalid_battery_time_estimates() {
        assert_eq!(duration_from_minutes(-1.0), Duration::ZERO);
        assert_eq!(duration_from_minutes(f32::NAN), Duration::ZERO);
        assert_eq!(duration_from_minutes(f32::INFINITY), Duration::ZERO);
        assert_eq!(duration_from_minutes(65_535.0), Duration::ZERO);
    }

    #[test]
    fn converts_reasonable_battery_time_estimates() {
        assert_eq!(duration_from_minutes(90.0), Duration::from_secs(5_400));
    }

    #[test]
    fn distinguishes_missing_sensor_values_from_real_zeroes() {
        let ioreg = IORegistry::default();
        let missing: NormalizedResource = (&ioreg, &SMCPowerData::default()).into();
        assert_eq!(missing.brightness_power, 0.0);
        assert_eq!(missing.heatpipe_power, 0.0);
        assert!(!missing.brightness_power_available);
        assert!(!missing.heatpipe_power_available);

        let available_zeroes: NormalizedResource = (
            &ioreg,
            &SMCPowerData {
                brightness_available: true,
                heatpipe_available: true,
                ..Default::default()
            },
        )
            .into();
        assert_eq!(available_zeroes.brightness_power, 0.0);
        assert_eq!(available_zeroes.heatpipe_power, 0.0);
        assert!(available_zeroes.brightness_power_available);
        assert!(available_zeroes.heatpipe_power_available);
    }
}

pub fn get_mac_ioreg() -> anyhow::Result<IORegistry> {
    let dic = get_mac_ioreg_dict()?;
    Ok(dict_into::<repr::IORegistry>(dic)?.into())
}

#[derive(Debug)]
pub struct MergedPowerData {
    pub from: PowerDataFrom,
    pub smc: Option<SMCPowerData>,
    pub ioreg: IORegistry,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PowerDataFrom {
    #[default]
    Local,
    Remote((String, String, InterfaceType)),
}

impl Deref for MergedPowerData {
    type Target = IORegistry;

    fn deref(&self) -> &Self::Target {
        &self.ioreg
    }
}

#[derive(Debug, Default)]
pub struct PowerStatistic {
    pub max_battery_power: f32,
    pub max_input_power: f32,
    pub max_system_power: f32,

    pub battery_history: VecDeque<u64>,
    pub input_history: VecDeque<u64>,
    pub system_history: VecDeque<u64>,
}

impl PowerStatistic {
    pub fn update(&mut self, battery_power: f32, input_power: f32, system_power: f32) {
        if battery_power > self.max_battery_power {
            self.max_battery_power = battery_power;
        }

        if input_power > self.max_input_power {
            self.max_input_power = input_power;
        }

        if system_power > self.max_system_power {
            self.max_system_power = system_power;
        }

        self.battery_history.push_back(battery_power.abs() as u64);
        if self.battery_history.len() > 50 {
            self.battery_history.pop_front();
        }

        self.input_history.push_back(input_power.abs() as u64);
        if self.input_history.len() > 50 {
            self.input_history.pop_front();
        }

        self.system_history.push_back(system_power.abs() as u64);
        if self.system_history.len() > 200 {
            self.system_history.pop_front();
        }
    }

    pub fn battery_history(&self, width: usize) -> Vec<SparklineBar> {
        skip_until(self.battery_history.iter(), width)
            .map(|v| SparklineBar::from(*v))
            .collect()
    }

    pub fn input_history(&self, width: usize) -> Vec<SparklineBar> {
        skip_until(self.input_history.iter(), width)
            .map(|v| SparklineBar::from(*v))
            .collect()
    }

    pub fn system_history(&self, width: usize) -> Vec<SparklineBar> {
        skip_until(self.system_history.iter(), width)
            .map(|v| SparklineBar::from(*v))
            .collect()
    }
}
