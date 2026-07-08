//! Chainable seeder for pre-populating an [`crate::EscurelProcess`]
//! with skills, instances, and free-form pages.
//!
//! The committed shape is in `docs/spec/dx.md` §"Fixture/seeding
//! façade". The key invariant: a fixture call never bypasses the
//! public write path. Every entry the builder declares is replayed
//! after `spawn` via the same `update_page` call the production
//! write path uses, so what tests seed is what `update_page` would
//! seed in production.

use std::collections::HashMap;

/// Markdown body for a seeded page. Accepted as anything that
/// implements `Into<MarkdownBody>` so call sites can pass `&str`,
/// `String`, or `include_str!(...)` interchangeably.
#[derive(Debug, Clone)]
pub struct MarkdownBody(String);

impl MarkdownBody {
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl From<&str> for MarkdownBody {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for MarkdownBody {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A single seeded markdown entry. Stored as `(tenant, page_id,
/// body)` triples in declaration order so `spawn` replays them in
/// the order the test wrote them. Order matters for skills that
/// must exist before their instances do.
#[derive(Debug, Clone)]
pub(crate) struct FixtureEntry {
    pub(crate) tenant: String,
    pub(crate) page_id: String,
    pub(crate) body: String,
}

/// Top-level fixture builder. Chainable: `FixtureBuilder::new()
/// .tenant("acme").skill(...).instance(...).done()`.
///
/// `FixtureBuilder` is `Default` so tests can leave
/// `Opts::fixtures` as `None` when nothing needs seeding, and use
/// the builder otherwise.
#[derive(Debug, Default, Clone)]
pub struct FixtureBuilder {
    pub(crate) entries: Vec<FixtureEntry>,
    /// Every tenant a `.tenant(...)` scope was opened for, in order —
    /// recorded even when the scope seeds no pages, so the harness can bind
    /// its single-tenant indexer to the intended tenant (see
    /// `EscurelProcess::spawn`).
    pub(crate) declared_tenants: Vec<String>,
}

impl FixtureBuilder {
    /// Start an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a per-tenant scope. The returned [`TenantFixture`]
    /// carries the parent back via `.done()` so multi-tenant
    /// chains read top-down.
    #[must_use]
    pub fn tenant(mut self, id: &str) -> TenantFixture {
        self.declared_tenants.push(id.to_owned());
        TenantFixture {
            parent: self,
            tenant_id: id.to_owned(),
        }
    }

    /// Expose the recorded entries in declaration order. Used by
    /// [`crate::EscurelProcess::spawn`] to replay them through the
    /// public write path.
    pub(crate) fn into_entries(self) -> Vec<FixtureEntry> {
        // Last-write-wins per (tenant, page_id) — the spec's escape
        // hatch is `page()` and tests that call it twice expect the
        // later body. We preserve the *position* of the latest
        // write so order-sensitive seeding (skill before instance)
        // still holds.
        let mut seen: HashMap<(String, String), usize> = HashMap::new();
        let mut out: Vec<Option<FixtureEntry>> = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            let key = (entry.tenant.clone(), entry.page_id.clone());
            if let Some(&idx) = seen.get(&key) {
                out[idx] = None;
            }
            seen.insert(key, out.len());
            out.push(Some(entry));
        }
        out.into_iter().flatten().collect()
    }
}

/// Per-tenant scope opened by [`FixtureBuilder::tenant`]. All
/// `skill` / `instance` / `page` calls attach to this tenant until
/// [`TenantFixture::done`] returns to the parent.
#[derive(Debug)]
pub struct TenantFixture {
    parent: FixtureBuilder,
    tenant_id: String,
}

impl TenantFixture {
    /// Seed a skill page at `markdown/skills/<id>.md`.
    ///
    /// The body must be a valid skill markdown document (frontmatter
    /// with `type: skill`, `id: <id>`). The builder does no
    /// validation here — the gateway's `update_page` does it, and
    /// `spawn` will panic if seeding fails, which is the right
    /// behaviour for a test fixture.
    #[must_use]
    pub fn skill(mut self, id: &str, body: impl Into<MarkdownBody>) -> Self {
        self.parent.entries.push(FixtureEntry {
            tenant: self.tenant_id.clone(),
            page_id: format!("markdown/skills/{id}.md"),
            body: body.into().into_string(),
        });
        self
    }

    /// Seed an instance page at `markdown/instances/<skill>/<id>.md`.
    #[must_use]
    pub fn instance(mut self, skill: &str, id: &str, body: impl Into<MarkdownBody>) -> Self {
        self.parent.entries.push(FixtureEntry {
            tenant: self.tenant_id.clone(),
            page_id: format!("markdown/instances/{skill}/{id}.md"),
            body: body.into().into_string(),
        });
        self
    }

    /// Escape hatch: seed an arbitrary markdown page at `path`
    /// (relative to the tenant's `markdown/` root, no leading
    /// `markdown/` prefix needed). Use when the structured `skill`
    /// / `instance` helpers don't cover the case.
    #[must_use]
    pub fn page(mut self, path: &str, body: impl Into<MarkdownBody>) -> Self {
        let page_id = if path.starts_with("markdown/") {
            path.to_owned()
        } else {
            format!("markdown/{path}")
        };
        self.parent.entries.push(FixtureEntry {
            tenant: self.tenant_id.clone(),
            page_id,
            body: body.into().into_string(),
        });
        self
    }

    /// Close the tenant scope and return to the parent builder.
    #[must_use]
    pub fn done(self) -> FixtureBuilder {
        self.parent
    }
}
