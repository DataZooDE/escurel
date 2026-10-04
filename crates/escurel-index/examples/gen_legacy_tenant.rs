//! Generate a LEGACY tenant (the old `type:` page-kind key) of any size, on disk, for measuring the
//! `type:` -> `kind:` migration and the boot scan at scale:
//!
//! ```sh
//! cargo run --release -p escurel-index --example gen_legacy_tenant -- \
//!     <data_dir> [tenant=default] [pages=20000] [drafts=2000] [snapshots=500]
//! ```
//!
//! The result is what `escurel-server` boots over (`ESCUREL_SERVER_DATA_DIR=<data_dir>`):
//! lane files under `<data_dir>/tenants/<tenant>/markdown/**` with `type:` frontmatter, a DuckDB file
//! `<data_dir>/tenants/<tenant>/escurel.duckdb` whose `drafts` rows carry legacy content (an engine
//! that validates drafts would refuse to create these, so they are inserted the way an old engine
//! stored them), and `crdt_snapshots` rows whose Loro snapshots hold legacy markdown.
//!
//! Nothing here is a mock: it is the on-disk shape of a store written before the hard cut.

use std::path::PathBuf;

use duckdb::{Connection, params};
use escurel_index::Migrator;
use sha2::{Digest as _, Sha256};

const SKILLS: usize = 20;

fn arg<T: std::str::FromStr>(n: usize, default: T) -> T {
    std::env::args()
        .nth(n)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn instance(i: usize) -> String {
    format!(
        "---\ntype: instance\nskill: s{s}\nid: i{i}\nstatus: open\n---\n# Instance {i}\n\n\
         Body of instance {i}; the prose line type: instance must survive.\n",
        s = i % SKILLS
    )
}

fn main() {
    let data_dir = PathBuf::from(std::env::args().nth(1).expect("usage: <data_dir> ..."));
    let tenant: String = arg(2, "default".to_owned());
    let pages: usize = arg(3, 20_000);
    let drafts: usize = arg(4, 2_000);
    let snapshots: usize = arg(5, 500);

    let tenant_dir = data_dir.join("tenants").join(&tenant);
    for s in 0..SKILLS {
        let dir = tenant_dir.join("markdown/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("s{s}.md")),
            format!("---\ntype: skill\nid: s{s}\ndescription: skill {s}\n---\n# s{s}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(tenant_dir.join(format!("markdown/instances/s{s}"))).unwrap();
    }
    for i in 0..pages {
        std::fs::write(
            tenant_dir.join(format!("markdown/instances/s{}/i{i}.md", i % SKILLS)),
            instance(i),
        )
        .unwrap();
    }

    let conn = Connection::open(tenant_dir.join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    for d in 0..drafts {
        let content = instance(d);
        let sha = hex(&Sha256::digest(content.as_bytes()));
        conn.execute(
            "INSERT INTO drafts (draft_id, target_page_id, content, content_sha256, author, status) \
             VALUES (?, ?, ?, ?, 'agent:gen', 'open')",
            params![
                format!("draft-{d:06}"),
                format!("markdown/instances/s{}/i{d}.md", d % SKILLS),
                content,
                sha
            ],
        )
        .unwrap();
    }
    for n in 0..snapshots {
        let bytes = escurel_crdt::snapshot_bytes_from_markdown(&instance(n)).unwrap();
        conn.execute(
            "INSERT INTO crdt_snapshots (page_id, snapshot_hlc, snapshot_bytes) VALUES (?, ?, ?)",
            params![
                format!("markdown/instances/s{}/i{n}.md", n % SKILLS),
                i64::try_from(n).unwrap() + 1,
                bytes
            ],
        )
        .unwrap();
    }
    conn.execute_batch("CHECKPOINT").unwrap();
    println!(
        "generated {} lane pages + {SKILLS} skills, {drafts} open drafts, {snapshots} snapshots under {}",
        pages,
        tenant_dir.display()
    );
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
