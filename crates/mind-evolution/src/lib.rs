//! mind-evolution — thin calibration/prediction ledger over yantrikdb-core engines

#[cfg(test)]
mod tests {
    #[test]
    fn self_build_requeues_invalid_authentication_credentials() {
        let tick = include_str!("../../../deploy/self_build_tick.sh");
        let classifier = tick
            .lines()
            .find(|line| line.contains("grep -qiE"))
            .expect("self-build tick must classify unavailable-builder output");

        assert!(classifier.contains("invalid authentication credentials"));
    }
}
