use crate::weather::Reading;

/// Average the available temperature samples.
pub fn average_temperature(samples: &[Reading]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }

    let total: f64 = samples.iter().map(|reading| reading.temperature).sum();
    Some(total / samples.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::average_temperature;

    #[test]
    fn empty_forecast_has_no_average() {
        assert_eq!(average_temperature(&[]), None);
    }
}
