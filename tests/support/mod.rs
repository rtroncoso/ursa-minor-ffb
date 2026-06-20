// Scripted SimConnect main-data timeline for integration tests.
pub fn scripted_flight_timeline() -> Vec<([f64; 8], bool)> {
    vec![
        ([0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], false),
        ([0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.05, 0.0], false),
        ([120.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0, 0.0], false),
        ([150.0, 0.0, 0.0, 50.0, 50.0, 2.0, 2.0, 0.0], false),
        ([70.0, 0.0, 25.0, 0.0, 0.0, 0.0, 3.0, 0.0], false),
        ([0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 3.1, 1.0], false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_has_expected_number_of_steps() {
        assert_eq!(scripted_flight_timeline().len(), 6);
    }
}
