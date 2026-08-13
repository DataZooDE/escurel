//! Audio blobs are first-class ingest content (CR-4, GH #356).
//!
//! Escurel is NOT asked to transcribe. The gap this pins is the *blob's*
//! place in the knowledge base: a recording shared into the corpus must
//! become an instance with identity, links, ACL and history — not a file a
//! consumer keeps a private pointer to. The transcript is supplied
//! separately by the caller (Heron does speech-to-text cloud-side, D12).
//!
//! So the shape under test is "retain, don't extract":
//!
//!   * a `document` skill may declare `accepts: ["audio/*"]` — a type
//!     wildcard, because `audio/` has a long tail of subtypes and an
//!     operator enumerating them all is how a recording silently parks;
//!   * an exact `accepts:` entry still beats a wildcard, so a wildcard
//!     skill can never steal a MIME another skill claimed by name;
//!   * the audio upload materialises (status `ok`, zero chunks) rather than
//!     parking `no_handler` or failing extraction, and the bytes are
//!     promoted to the canonical area and fetchable verbatim;
//!   * the overlay records the declared `content_type` and what can be read
//!     off the container deterministically (size, codec, duration);
//!   * the instance is an ordinary instance: it resolves by wikilink, other
//!     pages link to it, `list_instances` lists it, `update_page` versions
//!     it, and the skill's ACL decides who reads it.
//!
//! Real gateway + real Indexer + real DuckDB + real OIDC over real HTTP.
//! No mocks. Every negative assertion is paired with the positive control
//! that would catch a "nothing routes / everything is hidden" regression.

use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "heron";

// The wildcard skill: any `audio/*` recording lands here. Quoted because a
// bare `*` after `/` is still a plain YAML scalar but the quotes make the
// intent unambiguous to a human reading the fixture.
const RECORDING_SKILL: &str = "\
---
type: skill
id: recording
description: Audio recordings retained as evidence behind derived records.
backend:
  kind: document
  accepts: [\"audio/*\"]
---
# recording
";

// A skill that claims ONE audio MIME by name. Its existence is what makes
// the wildcard rule falsifiable: `audio/mpeg` must reach this skill, not
// the alphabetically-earlier wildcard one.
const PODCAST_SKILL: &str = "\
---
type: skill
id: podcast
description: MP3 episodes, claimed by exact MIME.
backend:
  kind: document
  accepts: [audio/mpeg]
---
# podcast
";

// A group-scoped audio skill: the uploader owns it, the team reads it.
// Reached by explicit `skill:` (MIME routing would pick `podcast`/`recording`).
const PRIVATE_SKILL: &str = "\
---
type: skill
id: recording_team
description: Team-internal recordings.
owner_field: author
acl:
  read: [owner, team:rekorder]
  create: [owner, team:rekorder]
backend:
  kind: document
  accepts: [\"audio/*\"]
---
# recording_team
";

// A text skill, so the audio assertions have a same-pipeline control.
const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: Text memos.
backend:
  kind: document
  accepts: [text/plain]
---
# memo
";

struct Setup {
    process: EscurelProcess,
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    for (id, body) in [
        ("recording", RECORDING_SKILL),
        ("podcast", PODCAST_SKILL),
        ("recording_team", PRIVATE_SKILL),
        ("memo", MEMO_SKILL),
    ] {
        indexer
            .update_page(&format!("markdown/skills/{id}.md"), body)
            .await
            .unwrap();
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            // The binary always boots a CRDT backend; without it `expand`
            // omits `version` and the history assertion below is vacuous.
            live_crdt: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    Setup {
        process,
        store,
        indexer,
        _dirs: vec![store_dir, db_dir],
    }
}

/// A real, minimal 16-bit mono PCM WAV: a 44-byte RIFF/WAVE header over
/// `samples` frames at 16 kHz. Not a fixture blob — the bytes are built
/// here so the expected duration is arithmetic the reader can check.
fn wav(samples: usize) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    let block_align = CHANNELS * BITS / 8;
    let byte_rate = SAMPLE_RATE * u32::from(block_align);
    let data_len = samples * block_align as usize;
    let mut b = Vec::with_capacity(44 + data_len);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&CHANNELS.to_le_bytes());
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&byte_rate.to_le_bytes());
    b.extend_from_slice(&block_align.to_le_bytes());
    b.extend_from_slice(&BITS.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    // A non-silent ramp, so the payload is not accidentally all-zero.
    for i in 0..data_len {
        b.push((i % 251) as u8);
    }
    b
}

/// `POST /ingest/upload` — the browser-facing intake named in the issue's
/// acceptance criteria.
async fn upload(
    p: &EscurelProcess,
    token: &str,
    bytes: &[u8],
    ct: &str,
    skill: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let mut body = json!({
        "content_type": ct,
        "bytes_b64": base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    if let Some(sk) = skill {
        body["skill"] = json!(sk);
    }
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest/upload", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["result"]["structuredContent"].clone()
}

/// The acceptance criterion, stated directly: an `audio/*` upload does not
/// park with `no_handler`. The `video/mp4` leg is the control — without it
/// this test would also pass if the pipeline started accepting everything,
/// which would route uploads into skills that never claimed them.
#[tokio::test]
async fn an_audio_upload_materialises_instead_of_parking_no_handler() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let (status, resp) = upload(&s.process, &token, &wav(8_000), "audio/wav", None).await;
    assert_eq!(status, 202, "audio upload accepted: {resp}");
    assert_eq!(
        resp["status"], "materialised",
        "an audio/* upload must become an instance, not park: {resp}"
    );
    assert_eq!(resp["handler_skill"], "recording", "resp: {resp}");
    assert_eq!(
        resp["chunk_count"].as_u64().unwrap(),
        0,
        "retain-only: escurel does not transcribe, so there are no chunks: {resp}"
    );

    // CONTROL: a MIME no skill accepts still parks, blob retained. The
    // wildcard is `audio/*`, not `*/*`.
    let (vstatus, video) = upload(
        &s.process,
        &token,
        b"\x00\x00\x00\x18ftypmp42",
        "video/mp4",
        None,
    )
    .await;
    assert_eq!(vstatus, 202, "{video}");
    assert_eq!(
        video["status"], "no_handler",
        "video/mp4 is claimed by no skill and must still park: {video}"
    );

    s.process.shutdown().await;
}

/// An exact `accepts:` entry beats a wildcard one. Without this, adding a
/// wildcard skill would silently capture MIMEs another skill claimed by
/// name — an upload landing in the wrong, possibly wider-visible collection.
#[tokio::test]
async fn an_exact_mime_claim_beats_a_wildcard_one() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    // `podcast` claims audio/mpeg by name; `recording` claims audio/* and
    // sorts EARLIER, so id-order alone would pick the wrong one.
    let (_, mp3) = upload(
        &s.process,
        &token,
        b"ID3\x04\x00\x00\x00\x00\x00\x00",
        "audio/mpeg",
        None,
    )
    .await;
    assert_eq!(
        mp3["handler_skill"], "podcast",
        "the exact claim must win over the wildcard: {mp3}"
    );

    // CONTROL: a subtype nobody claims by name still reaches the wildcard
    // skill — the exact-match preference must not disable wildcards.
    let (_, other) = upload(&s.process, &token, &wav(1_000), "audio/wav", None).await;
    assert_eq!(
        other["handler_skill"], "recording",
        "an unclaimed audio subtype falls through to the wildcard: {other}"
    );

    s.process.shutdown().await;
}

/// The overlay is the recording's record: the retained blob, the declared
/// content type, and what the container yields deterministically.
#[tokio::test]
async fn the_overlay_records_the_content_type_and_media_metadata() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // 8000 frames at 16 kHz = exactly 500 ms.
    let bytes = wav(8_000);

    let (_, resp) = upload(&s.process, &token, &bytes, "audio/wav", None).await;
    let page_id = resp["page_id"].as_str().expect("page_id").to_owned();

    let page = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    let bref = &page["frontmatter"]["backend_ref"];
    assert_eq!(bref["kind"], "document", "page: {page}");
    assert_eq!(
        bref["status"], "ok",
        "retain-only is a success, not an extraction failure: {page}"
    );
    assert_eq!(
        bref["content_type"], "audio/wav",
        "the declared MIME is recorded, so a client can serve it back: {page}"
    );
    let ex = &bref["extracted"];
    assert_eq!(
        ex["bytes"].as_u64().unwrap(),
        bytes.len() as u64,
        "size: {page}"
    );
    assert_eq!(ex["codec"], "wav", "codec from the declared MIME: {page}");
    assert_eq!(
        ex["duration_ms"].as_u64().unwrap(),
        500,
        "8000 frames at 16 kHz is 500 ms: {page}"
    );

    s.process.shutdown().await;
}

/// The bytes survive: promoted to the canonical area (not left in the
/// inbox) and served back verbatim with the declared type, so a client can
/// play the recording rather than download an `application/octet-stream`.
#[tokio::test]
async fn the_audio_bytes_are_retained_and_fetchable_verbatim() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let bytes = wav(4_000);

    let (_, resp) = upload(&s.process, &token, &bytes, "audio/wav", None).await;
    let page_id = resp["page_id"].as_str().unwrap().to_owned();

    let got = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({ "page_id": page_id }),
    )
    .await;
    let blob = &got["blob"];
    assert_eq!(
        blob["content_type"], "audio/wav",
        "sniffing a RIFF header yields octet-stream; the declared type must win: {got}"
    );
    assert_eq!(blob["size"].as_u64().unwrap(), bytes.len() as u64, "{got}");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(blob["bytes_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, bytes, "bytes must round-trip byte-for-byte");

    // CONTROL: the text path is unchanged — a text/plain upload still
    // reports text/plain and its own bytes.
    let (_, txt) = upload(&s.process, &token, b"a plain memo", "text/plain", None).await;
    let tpage = txt["page_id"].as_str().unwrap().to_owned();
    let tgot = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({ "page_id": tpage }),
    )
    .await;
    assert_eq!(tgot["blob"]["content_type"], "text/plain", "{tgot}");

    s.process.shutdown().await;
}

/// "Ordinary identity, links and history" — the part that distinguishes an
/// instance from a file with a pointer to it.
#[tokio::test]
async fn the_recording_is_an_ordinary_instance_with_identity_links_and_history() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let (_, resp) = upload(&s.process, &token, &wav(2_000), "audio/wav", None).await;
    let page_id = resp["page_id"].as_str().unwrap().to_owned();
    let instance_id = page_id
        .rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".md"))
        .expect("instance id")
        .to_owned();

    // IDENTITY: it is listed as an instance of its skill…
    let listed = call(
        &s.process,
        &token,
        "list_instances",
        json!({ "skill_id": "recording" }),
    )
    .await;
    let ids: Vec<String> = listed["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .filter_map(|i| i["page_id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        ids.contains(&page_id),
        "the recording must be listed among its skill's instances: {ids:?}"
    );

    // …and it resolves by wikilink, like any other instance.
    let r = call(
        &s.process,
        &token,
        "resolve",
        json!({ "wikilink": format!("[[recording::{instance_id}]]") }),
    )
    .await;
    assert_eq!(
        r["page"]["page_id"], page_id,
        "the recording must resolve by wikilink: {r}"
    );

    // LINKS: the derived record — the transcript Heron files separately —
    // links to the recording, and the edge is traversable back.
    s.indexer
        .update_page(
            "markdown/instances/memo/transkript.md",
            &format!(
                "---\ntype: instance\nskill: memo\nid: transkript\n---\n\
                 # Transkript\nEvidence: [[recording::{instance_id}]].\n"
            ),
        )
        .await
        .unwrap();
    let nb = call(
        &s.process,
        &token,
        "neighbours",
        json!({ "page_id": page_id, "direction": "in" }),
    )
    .await;
    assert!(
        !nb["edges"].as_array().expect("edges").is_empty(),
        "the derived record's link to the recording must be traversable: {nb}"
    );

    // HISTORY: the recording is a versioned page on the CRDT lane like any
    // other instance, and re-ingesting the same bytes is a true no-op —
    // same instance, no version churn. That idempotence is what lets a
    // consumer retry a flaky upload without forking the evidence.
    let before = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    assert!(
        before["version"].is_string(),
        "the recording must carry a monotonic version: {before}"
    );
    let (_, again) = upload(&s.process, &token, &wav(2_000), "audio/wav", None).await;
    assert_eq!(
        again["page_id"], page_id,
        "re-ingesting the same recording is idempotent in identity: {again}"
    );
    let after = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    assert_eq!(
        after["version"], before["version"],
        "an identical re-ingest must not churn the version: {after}"
    );

    // And the read-only document contract applies unchanged: a recording is
    // managed by the ingest pipeline, not by `update_page` — exactly as a
    // PDF instance is. Pinned because it is the first thing a consumer
    // filing an annotated recording will hit, and the answer is "annotate a
    // page that links to it", not "edit the overlay".
    let edited = format!(
        "---\ntype: instance\nskill: recording\nid: {instance_id}\n---\n\
         # Kundengespräch\nAnnotated after review.\n"
    );
    let write = call(
        &s.process,
        &token,
        "update_page",
        json!({ "page_id": page_id, "content": edited }),
    )
    .await;
    assert_eq!(write["ok"], false, "{write}");
    assert_eq!(
        write["issues"][0]["code"], "backend_read_only",
        "the document backend's read-only rule must apply to audio too: {write}"
    );

    s.process.shutdown().await;
}

/// ACL applies to a recording exactly as to any other document instance:
/// the group reads it, an outsider does not — and denial is absence, not a
/// distinguishable error.
#[tokio::test]
async fn a_recording_is_acl_scoped_like_any_other_instance() {
    let s = setup().await;
    let alice = s
        .process
        .mint_token_with_groups(TENANT, "alice", &["team:rekorder"], false);
    let carla = s
        .process
        .mint_token_with_groups(TENANT, "carla", &["team:rekorder"], false);
    let dora = s
        .process
        .mint_token_with_groups(TENANT, "dora", &["team:andere"], false);

    let (status, resp) = upload(
        &s.process,
        &alice,
        &wav(2_000),
        "audio/wav",
        Some("recording_team"),
    )
    .await;
    assert_eq!(status, 202, "{resp}");
    assert_eq!(
        resp["handler_skill"], "recording_team",
        "explicit skill honoured for audio too: {resp}"
    );
    let page_id = resp["page_id"].as_str().expect("page_id").to_owned();

    // POSITIVE CONTROLS: uploader and teammate both read it.
    for (who, tok) in [("alice", &alice), ("carla", &carla)] {
        let p = call(&s.process, tok, "expand", json!({ "page_id": page_id })).await;
        assert!(p["page"].is_object(), "{who} must read the recording: {p}");
    }

    // The outsider sees neither the page nor its bytes.
    let hidden = call(&s.process, &dora, "expand", json!({ "page_id": page_id })).await;
    assert!(
        hidden["page"].is_null(),
        "dora must not read another team's recording: {hidden}"
    );
    let blob = call(
        &s.process,
        &dora,
        "fetch_blob",
        json!({ "page_id": page_id }),
    )
    .await;
    assert!(
        blob["blob"].is_null(),
        "nor fetch its bytes around the page ACL: {blob}"
    );

    s.process.shutdown().await;
}

/// The pre-existing failure contract is untouched: a blob that a real
/// extractor rejects still retains the upload and marks the instance
/// `extraction_failed` (the issue's third acceptance criterion, asserted
/// here so the retain-only path cannot be mistaken for having replaced it).
#[tokio::test]
async fn extraction_failure_still_retains_the_upload() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // Invalid UTF-8 declared as text/plain → PlainTextExtractor fails.
    let bad = Bytes::from_static(&[0xff, 0xfe, 0x00, 0x01]);
    let blob = s
        .store
        .put_inbox_blob(TENANT, bad.clone(), None)
        .await
        .unwrap();
    let (_, resp) = upload(&s.process, &token, &bad, "text/plain", None).await;
    assert_eq!(
        resp["status"], "extraction_failed",
        "the text path's failure contract is unchanged: {resp}"
    );
    assert!(
        s.store.get_inbox_blob(TENANT, &blob).await.is_ok(),
        "the inbox blob must be retained on extraction failure"
    );

    s.process.shutdown().await;
}
