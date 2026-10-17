//! The Drive-friendly settings a gdrive-backed lake sets on its DuckDB connection, against the REAL `gdrive`
//! extension (no Drive call is made: the settings are read back from `duckdb_settings()`).
//!
//! `#[ignore]`d like every test that needs a built `gdrive.duckdb_extension`, because CI has none. Run with
//! `ESCUREL_TEST_GDRIVE_EXTENSION=/path/to/gdrive.duckdb_extension cargo test -p escurel-index --test suite
//! gdrive_lake_settings -- --ignored`. The v1.5.6 build is served at
//! `https://get.erpl.io/v1.5.6/linux_amd64/gdrive.duckdb_extension.gz`.

use duckdb::{Config, Connection};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, install_load_sql};

fn extension() -> String {
    std::env::var("ESCUREL_TEST_GDRIVE_EXTENSION")
        .expect("set ESCUREL_TEST_GDRIVE_EXTENSION to a built gdrive.duckdb_extension")
}

fn cfg(permanent_delete: bool) -> LakeConfig {
    LakeConfig {
        catalog_dsn: "host=127.0.0.1 port=5432 user=u password=p dbname=d".to_owned(),
        data_path: "gdrive://escurel-lake/lake".to_owned(),
        object_store: ObjectStoreSecret::Gdrive {
            drive_id: "0AA5vtjzlyjnoUk9PVA".to_owned(),
            scope: "https://www.googleapis.com/auth/drive".to_owned(),
            extension_path: Some(extension()),
            permanent_delete,
        },
    }
}

/// Only the gdrive part of the prelude: the ducklake/postgres INSTALLs need the network and are not under test.
fn gdrive_statements(cfg: &LakeConfig) -> String {
    install_load_sql(cfg)
        .split_inclusive(';')
        .filter(|s| s.contains("gdrive"))
        .collect::<String>()
}

fn setting(conn: &Connection, name: &str) -> String {
    conn.query_row(
        "SELECT value FROM duckdb_settings() WHERE name = ?",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn connection() -> Connection {
    Connection::open_in_memory_with_flags(Config::default().allow_unsigned_extensions().unwrap())
        .unwrap()
}

#[test]
#[ignore = "needs ESCUREL_TEST_GDRIVE_EXTENSION (a built gdrive.duckdb_extension)"]
fn the_lake_prelude_sets_the_real_extension_options_and_the_trash_stays_the_default() {
    let conn = connection();
    conn.execute_batch(&gdrive_statements(&cfg(false)))
        .expect("the real extension accepts the prelude (the options exist)");

    assert_eq!(
        setting(&conn, "gdrive_immutable_prefixes"),
        "gdrive://escurel-lake/lake"
    );
    assert_eq!(setting(&conn, "gdrive_permanent_delete"), "false");
}

#[test]
#[ignore = "needs ESCUREL_TEST_GDRIVE_EXTENSION (a built gdrive.duckdb_extension)"]
fn permanent_delete_is_set_only_when_asked_for() {
    let conn = connection();
    conn.execute_batch(&gdrive_statements(&cfg(true))).unwrap();
    assert_eq!(setting(&conn, "gdrive_permanent_delete"), "true");
}
