//! `ESCUREL_STORAGE_DUCKVFS_RETRY_*`: how the duckvfs lane store repeats a transient error.
//!
//! Config parsing needs no DuckDB, so these run in every build. A Drive lane answers an occasional
//! "temporary Drive API failure"; with no retry one such answer failed a 20 minute cold boot.

use std::collections::HashMap;
use std::time::Duration;

use escurel_server::EscurelConfig;
use tempfile::TempDir;

fn load(pairs: &[(&str, &str)]) -> Result<EscurelConfig, String> {
    let data = TempDir::new().unwrap();
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert("ESCUREL_STORAGE_BACKEND".into(), "duckvfs".into());
    map.insert(
        "ESCUREL_STORAGE_DUCKVFS_ROOT".into(),
        "gdrive://escurel/lanes".into(),
    );
    map.insert(
        "ESCUREL_STORAGE_DUCKVFS_DRIVE_ID".into(),
        "0AAexample".into(),
    );
    map.insert(
        "ESCUREL_SERVER_DATA_DIR".into(),
        data.path().display().to_string(),
    );
    for (k, v) in pairs {
        map.insert((*k).to_owned(), (*v).to_owned());
    }
    EscurelConfig::from_source(&move |k: &str| map.get(k).cloned()).map_err(|e| e.to_string())
}

#[test]
fn the_defaults_retry_six_times_with_a_thirty_second_cap_and_a_three_minute_budget() {
    let cfg = load(&[]).expect("config");
    let r = cfg.duckvfs.expect("duckvfs").retry;
    assert_eq!(r.attempts, 6);
    assert_eq!(r.max_backoff, Duration::from_secs(30));
    assert_eq!(r.total, Duration::from_secs(180));
}

#[test]
fn the_knobs_override_the_defaults() {
    let cfg = load(&[
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_ATTEMPTS", "3"),
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_MAX_BACKOFF_MS", "1500"),
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_TOTAL_MS", "9000"),
    ])
    .expect("config");
    let r = cfg.duckvfs.expect("duckvfs").retry;
    assert_eq!(r.attempts, 3);
    assert_eq!(r.max_backoff, Duration::from_millis(1500));
    assert_eq!(r.total, Duration::from_millis(9000));
}

#[test]
fn one_attempt_is_allowed_and_turns_retrying_off() {
    let cfg = load(&[("ESCUREL_STORAGE_DUCKVFS_RETRY_ATTEMPTS", "1")]).expect("config");
    assert_eq!(cfg.duckvfs.expect("duckvfs").retry.attempts, 1);
}

#[test]
fn nonsense_is_refused_naming_the_variable() {
    for (var, bad) in [
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_ATTEMPTS", "0"),
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_ATTEMPTS", "many"),
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_MAX_BACKOFF_MS", "-1"),
        ("ESCUREL_STORAGE_DUCKVFS_RETRY_TOTAL_MS", "soon"),
    ] {
        let err = load(&[(var, bad)]).expect_err(bad);
        assert!(err.contains(var), "{err}");
    }
}
