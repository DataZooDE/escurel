//! `ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_*`: the env surface of a `sharepoint://` lane store.
//!
//! Config parsing needs no DuckDB and no network, so these run in every build. What they pin:
//! a `sharepoint://` root fails closed when any credential or the drive/site target is missing,
//! the client secret can come from a file (so a pod mounts it rather than putting it in the
//! environment), and the secret never shows up in `Debug` output or in an error.

use std::collections::HashMap;
use std::io::Write;

use escurel_server::EscurelConfig;
use tempfile::TempDir;

const SECRET: &str = "s3cr3t-value-from-test";

/// The error is rendered to its message: that is all these tests assert on, and the typed
/// `ConfigError` is large enough to trip `result_large_err`.
fn load(pairs: &[(&str, &str)]) -> Result<EscurelConfig, String> {
    let data = TempDir::new().unwrap();
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert("ESCUREL_STORAGE_BACKEND".into(), "duckvfs".into());
    map.insert(
        "ESCUREL_SERVER_DATA_DIR".into(),
        data.path().display().to_string(),
    );
    for (k, v) in pairs {
        map.insert((*k).to_owned(), (*v).to_owned());
    }
    EscurelConfig::from_source(&move |k: &str| map.get(k).cloned()).map_err(|e| e.to_string())
}

const ROOT: (&str, &str) = (
    "ESCUREL_STORAGE_DUCKVFS_ROOT",
    "sharepoint://escurel-rehearsal/lanes",
);
const TENANT: (&str, &str) = (
    "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_TENANT_ID",
    "00000000-0000-0000-0000-000000000000",
);
const CLIENT: (&str, &str) = (
    "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_ID",
    "11111111-1111-1111-1111-111111111111",
);
const SECRET_ENV: (&str, &str) = ("ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_SECRET", SECRET);
const DRIVE: (&str, &str) = ("ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_DRIVE_ID", "b!AbCdEf");

#[test]
fn a_complete_sharepoint_config_parses_and_carries_the_credentials() {
    let cfg = load(&[ROOT, TENANT, CLIENT, SECRET_ENV, DRIVE]).expect("complete config");
    let sp = cfg
        .duckvfs
        .as_ref()
        .and_then(|d| d.sharepoint.as_ref())
        .expect("sharepoint credentials present");
    assert_eq!(sp.tenant_id, TENANT.1);
    assert_eq!(sp.client_id, CLIENT.1);
    assert_eq!(sp.client_secret.expose(), SECRET);
    assert_eq!(sp.drive_id.as_deref(), Some("b!AbCdEf"));
    assert!(sp.site_id.is_none());
}

#[test]
fn a_site_id_is_an_alternative_target() {
    let cfg = load(&[
        ROOT,
        TENANT,
        CLIENT,
        SECRET_ENV,
        (
            "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_SITE_ID",
            "contoso.sharepoint.com,site,web",
        ),
    ])
    .expect("site id alone");
    let sp = cfg.duckvfs.unwrap().sharepoint.unwrap();
    assert_eq!(
        sp.site_id.as_deref(),
        Some("contoso.sharepoint.com,site,web")
    );
}

#[test]
fn every_missing_field_fails_closed_and_names_the_variable() {
    let all = [TENANT, CLIENT, SECRET_ENV, DRIVE];
    for (i, (var, _)) in all.iter().enumerate() {
        let mut pairs = vec![ROOT];
        pairs.extend(
            all.iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, p)| *p),
        );
        let err = load(&pairs).expect_err("missing field must fail closed");
        let msg = err;
        // The target is "drive OR site", so its message names both.
        let needle = if var.ends_with("DRIVE_ID") {
            "SHAREPOINT_DRIVE_ID"
        } else {
            var
        };
        assert!(msg.contains(needle), "{var}: {msg}");
        assert!(!msg.contains(SECRET), "{msg}");
    }
}

#[test]
fn both_a_drive_and_a_site_are_refused_as_ambiguous() {
    let err = load(&[
        ROOT,
        TENANT,
        CLIENT,
        SECRET_ENV,
        DRIVE,
        (
            "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_SITE_ID",
            "contoso.sharepoint.com,site,web",
        ),
    ])
    .expect_err("ambiguous target");
    assert!(err.contains("SITE_ID"), "{err}");
}

#[test]
fn the_secret_can_come_from_a_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client-secret");
    let mut f = std::fs::File::create(&path).unwrap();
    // A mounted secret file usually ends in a newline.
    writeln!(f, "{SECRET}").unwrap();
    let cfg = load(&[
        ROOT,
        TENANT,
        CLIENT,
        DRIVE,
        (
            "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_SECRET_FILE",
            path.to_str().unwrap(),
        ),
    ])
    .expect("secret from file");
    let sp = cfg.duckvfs.unwrap().sharepoint.unwrap();
    assert_eq!(
        sp.client_secret.expose(),
        SECRET,
        "trailing newline trimmed"
    );
}

#[test]
fn an_unreadable_secret_file_is_an_error_that_names_the_variable_not_the_secret() {
    let err = load(&[
        ROOT,
        TENANT,
        CLIENT,
        DRIVE,
        (
            "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_SECRET_FILE",
            "/nonexistent/client-secret",
        ),
    ])
    .expect_err("missing file");
    assert!(err.contains("CLIENT_SECRET_FILE"), "{err}");
}

#[test]
fn a_secret_and_a_secret_file_together_are_refused() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client-secret");
    std::fs::write(&path, SECRET).unwrap();
    let err = load(&[
        ROOT,
        TENANT,
        CLIENT,
        DRIVE,
        SECRET_ENV,
        (
            "ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_SECRET_FILE",
            path.to_str().unwrap(),
        ),
    ])
    .expect_err("ambiguous secret source");
    assert!(err.contains("CLIENT_SECRET"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
}

#[test]
fn the_secret_never_appears_in_a_config_dump() {
    let cfg = load(&[ROOT, TENANT, CLIENT, SECRET_ENV, DRIVE]).unwrap();
    for rendered in [format!("{cfg:?}"), format!("{:#?}", cfg.duckvfs)] {
        assert!(
            !rendered.contains(SECRET),
            "secret leaked into Debug output"
        );
    }
}

#[test]
fn sharepoint_variables_on_a_non_sharepoint_root_are_refused() {
    let err = load(&[
        ("ESCUREL_STORAGE_DUCKVFS_ROOT", "gdrive://escurel/lanes"),
        ("ESCUREL_STORAGE_DUCKVFS_DRIVE_ID", "0AA5vtjzlyjnoUk9PVA"),
        SECRET_ENV,
    ])
    .expect_err("a stray credential must not ride along");
    assert!(err.contains("SHAREPOINT"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
}

#[test]
fn the_gdrive_requirements_are_unchanged() {
    // A gdrive:// root still demands its drive id, and needs no sharepoint field.
    let err = load(&[("ESCUREL_STORAGE_DUCKVFS_ROOT", "gdrive://escurel/lanes")])
        .expect_err("gdrive without a drive id");
    assert!(err.contains("DRIVE_ID"), "{err}");
    let ok = load(&[
        ("ESCUREL_STORAGE_DUCKVFS_ROOT", "gdrive://escurel/lanes"),
        ("ESCUREL_STORAGE_DUCKVFS_DRIVE_ID", "0AA5vtjzlyjnoUk9PVA"),
    ])
    .expect("gdrive config");
    assert!(ok.duckvfs.unwrap().sharepoint.is_none());
}
