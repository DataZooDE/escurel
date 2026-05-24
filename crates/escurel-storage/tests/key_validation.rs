//! Integration tests for [`escurel_storage::Key`] input validation.
//!
//! The store maps `Key { tenant, path }` directly onto filesystem
//! paths; any caller that can put `..` segments or absolute paths
//! into a `Key` can cross tenant boundaries or escape the store
//! root entirely. Validation has to live at the `Key` boundary so
//! that once a `Key` exists, every downstream consumer (`FsStore`,
//! the eventual `S3Store`, `Indexer::audit`, …) can trust it.
//!
//! These tests exercise the public `Key::new` API directly — no
//! mocks, no FsStore involvement.

use escurel_storage::{Key, KeyError};

// --- valid inputs -----------------------------------------------

#[test]
fn accepts_normal_tenant_and_path() {
    Key::new("acme", "markdown/skills/customer.md").expect("standard key is valid");
}

#[test]
fn accepts_empty_path_for_full_tenant_prefix() {
    // `list(Key::new(tenant, ""))` is the documented "all keys for
    // this tenant" call (used by `list_isolates_tenants`).
    Key::new("acme", "").expect("empty path is a valid full-tenant prefix");
}

#[test]
fn accepts_dotfile_names() {
    Key::new("acme", "markdown/.gitkeep").expect(".gitkeep is a normal file");
}

#[test]
fn accepts_dot_dot_inside_a_segment() {
    // `a..b` is a normal filename, NOT a `..` segment.
    Key::new("acme", "markdown/a..b/file.md").expect("`..` in name is allowed");
}

// --- tenant rejections ------------------------------------------

#[test]
fn rejects_empty_tenant() {
    let err = Key::new("", "x").expect_err("empty tenant must be rejected");
    assert!(matches!(err, KeyError::InvalidTenant(_)));
}

#[test]
fn rejects_tenant_with_forward_slash() {
    let err = Key::new("acme/globex", "x").expect_err("tenant with `/` must be rejected");
    assert!(matches!(err, KeyError::InvalidTenant(_)));
}

#[test]
fn rejects_tenant_with_backslash() {
    let err = Key::new("acme\\globex", "x").expect_err("tenant with `\\` must be rejected");
    assert!(matches!(err, KeyError::InvalidTenant(_)));
}

#[test]
fn rejects_tenant_dot_dot() {
    let err = Key::new("..", "x").expect_err("`..` tenant must be rejected");
    assert!(matches!(err, KeyError::InvalidTenant(_)));
}

#[test]
fn rejects_tenant_dot() {
    let err = Key::new(".", "x").expect_err("`.` tenant must be rejected");
    assert!(matches!(err, KeyError::InvalidTenant(_)));
}

// --- path rejections --------------------------------------------

#[test]
fn rejects_absolute_path() {
    let err = Key::new("acme", "/etc/passwd").expect_err("absolute path must be rejected");
    assert!(matches!(err, KeyError::InvalidPath(_)));
}

#[test]
fn rejects_path_with_dot_dot_segment_at_start() {
    let err =
        Key::new("acme", "../globex/manifest.toml").expect_err("`../` at start must be rejected");
    assert!(matches!(err, KeyError::InvalidPath(_)));
}

#[test]
fn rejects_path_with_dot_dot_segment_in_middle() {
    let err = Key::new("acme", "markdown/../../globex/manifest.toml")
        .expect_err("internal `..` segment must be rejected");
    assert!(matches!(err, KeyError::InvalidPath(_)));
}

#[test]
fn rejects_path_with_single_dot_segment() {
    let err = Key::new("acme", "markdown/./customer.md")
        .expect_err("internal `.` segment must be rejected");
    assert!(matches!(err, KeyError::InvalidPath(_)));
}

#[test]
fn rejects_path_with_backslash() {
    // Windows path separator — don't let it sneak past the unix-only
    // `..` segment check by hiding inside a "name".
    let err =
        Key::new("acme", "markdown\\skills\\x.md").expect_err("backslash in path must be rejected");
    assert!(matches!(err, KeyError::InvalidPath(_)));
}

// --- prefix semantics keep working ------------------------------

#[test]
fn has_prefix_still_works_after_validation() {
    let parent = Key::new("acme", "markdown/").expect("parent");
    let child = Key::new("acme", "markdown/skills/customer.md").expect("child");
    assert!(child.has_prefix(&parent));
    assert!(!parent.has_prefix(&child));
}
