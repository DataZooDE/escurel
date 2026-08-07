//! The object-key layout shared by every object-store backend.
//!
//! `s3.rs` and `gcs.rs` each carried their own `normalise_prefix`,
//! `object_key` and `list_prefix`. The first two were character-identical;
//! the third computed the same two strings by a different route, which is the
//! more dangerous kind of duplicate — it reads as though the backends
//! deliberately differ, and a reader has to derive that they don't.
//!
//! They must not differ. The layout is not an implementation detail of one
//! backend: it is where a tenant's bytes live, so a tenant moving between S3
//! and GCS (or a migration reading one and writing the other) depends on both
//! producing the same key for the same [`Key`]. That agreement now has one
//! implementation and a test, instead of being a coincidence maintained by
//! hand in two files.
//!
//! R8 of `docs/notes/complexity-reduction-plan.md`.

use crate::key::Key;

/// Strip leading and trailing separators from a configured store prefix.
///
/// `"/escurel/prod/"` and `"escurel/prod"` are the same location; normalising
/// on the way in is what lets the joins below stay simple.
#[must_use]
pub fn normalise_prefix(raw: &str) -> String {
    raw.trim_matches('/').to_owned()
}

/// The full object key for `key` under a store `prefix`.
///
/// `<prefix>/tenants/<tenant>/<path>`, or `tenants/<tenant>/<path>` when the
/// prefix is empty — no leading separator, which some backends treat as a
/// distinct (empty-named) top-level directory.
#[must_use]
pub fn object_key(prefix: &str, key: &Key) -> String {
    let tenant_path = format!("tenants/{}/{}", key.tenant(), key.path());
    if prefix.is_empty() {
        tenant_path
    } else {
        format!("{prefix}/{tenant_path}")
    }
}

/// `(full listing prefix, tenant base)` for a list operation.
///
/// The second element carries a trailing slash and is stripped off listed
/// object keys so callers get tenant-relative paths, which is what the
/// `LaneStore` contract requires.
#[must_use]
pub fn list_prefix(prefix: &str, key: &Key) -> (String, String) {
    let tenant_base = if prefix.is_empty() {
        format!("tenants/{}/", key.tenant())
    } else {
        format!("{prefix}/tenants/{}/", key.tenant())
    };
    let full = format!("{tenant_base}{}", key.path());
    (full, tenant_base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tenant: &str, path: &str) -> Key {
        Key::new(tenant, path).expect("valid key")
    }

    #[test]
    fn a_prefix_is_normalised_to_its_bare_form() {
        for raw in [
            "/escurel/prod/",
            "escurel/prod",
            "/escurel/prod",
            "escurel/prod/",
        ] {
            assert_eq!(normalise_prefix(raw), "escurel/prod", "input {raw:?}");
        }
        assert_eq!(normalise_prefix(""), "");
        assert_eq!(normalise_prefix("/"), "");
    }

    #[test]
    fn object_key_places_a_tenant_under_the_store_prefix() {
        assert_eq!(
            object_key("escurel/prod", &key("acme", "markdown/skills/customer.md")),
            "escurel/prod/tenants/acme/markdown/skills/customer.md"
        );
    }

    #[test]
    fn an_empty_prefix_yields_no_leading_separator() {
        // A leading `/` is not cosmetic: some backends treat it as an
        // empty-named top-level directory, so the bytes land somewhere else.
        let k = object_key("", &key("acme", "a.md"));
        assert_eq!(k, "tenants/acme/a.md");
        assert!(!k.starts_with('/'));

        let (full, base) = list_prefix("", &key("acme", "markdown/"));
        assert!(!full.starts_with('/') && !base.starts_with('/'));
    }

    #[test]
    fn a_list_base_ends_in_a_separator_so_it_strips_cleanly() {
        let (full, base) = list_prefix("escurel/prod", &key("acme", "markdown/skills/"));
        assert_eq!(full, "escurel/prod/tenants/acme/markdown/skills/");
        assert_eq!(base, "escurel/prod/tenants/acme/");
        assert!(base.ends_with('/'), "base must strip off cleanly: {base}");
        assert_eq!(full.strip_prefix(&base), Some("markdown/skills/"));
    }

    #[test]
    fn a_listing_prefix_extends_the_object_key_layout() {
        // The invariant that makes listing usable: the full listing prefix of
        // a directory is a prefix of the object key of anything inside it. If
        // these two functions ever disagree, listing silently returns nothing.
        let (full, _) = list_prefix("p", &key("acme", "markdown/"));
        let object = object_key("p", &key("acme", "markdown/skills/customer.md"));
        assert!(
            object.starts_with(&full),
            "object {object} must live under listing prefix {full}"
        );
    }
}
