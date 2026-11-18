use crate::weather::Reading;

/// Convert one station reading to a temperature.
pub fn first_temperature(sample: &Reading) -> f64 {
    sample.temperature
}
