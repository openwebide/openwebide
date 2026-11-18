use harbor_forecast::{forecast::average_temperature, weather::Reading};

#[test]
fn averages_the_available_readings() {
    let readings = [Reading { temperature: 11.0 }, Reading { temperature: 15.0 }];
    assert_eq!(average_temperature(&readings), Some(13.0));
}
