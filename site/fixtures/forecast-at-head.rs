use crate::weather::Reading;

/// Average the available temperature samples.
pub fn average_temperature(samples: &[Reading]) -> f64 {
    let total: f64 = samples.iter().map(|reading| reading.temperature).sum();
    total / samples.len() as f64
}
