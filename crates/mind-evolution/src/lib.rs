//! mind-evolution — thin calibration/prediction ledger over yantrikdb-core engines

#[cfg(test)]
mod tests {
    #[test]
    fn self_build_recognizes_invalid_authentication_credentials() {
        let helper = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/self_build_common.sh"
        );
        let status = std::process::Command::new("bash")
            .args([
                "-c",
                r#". "$1"; builder_unavailable "$2""#,
                "_",
                helper,
                "Failed to authenticate. API Error: 401 Invalid authentication credentials",
            ])
            .status()
            .expect("run self-build failure classifier");

        assert!(status.success(), "authentication failure must preserve the queued goal");
    }
}
