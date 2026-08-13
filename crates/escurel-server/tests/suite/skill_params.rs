//! The `params:` skill-page frontmatter key (escurel#358 / heron#11, CR-7),
//! over real HTTP against a running gateway with a real Indexer. No mocks,
//! no LLM.
//!
//! `required_frontmatter` is the shape of the instances a skill PRODUCES.
//! `params:` is the shape of what one RUN of the skill TAKES. For an
//! instance-creating skill the two nearly coincide, which is why the
//! distinction went unnoticed; for a report skill parameterised by window
//! and grouping they have nothing to do with each other.
//!
//! Escurel does not execute skills — it reports what the page declares, so a
//! client can build an input form from `list_skills` alone without expanding
//! every page.
//!
//! The load-bearing property here is the direction of failure, and it is the
//! OPPOSITE of `autonomy:`'s. An `autonomy:` value it cannot read is dropped,
//! because only an explicit `auto` may switch a human gate off. A param whose
//! `kind:` it cannot read is still REPORTED, as `string` — dropping it would
//! silently remove a required field from a generated form, and the run then
//! fails at invocation time with nothing on the page to explain why. An
//! over-permissive text box under-validates; a missing box loses data.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

// The control: a skill that declares no `params:` at all. Every existing
// skill page in every existing tenant looks like this.
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n---\n# note\n";

// The issue's motivating example — a report skill whose run inputs are
// nothing like its instance frontmatter. Declared in the LIST form, which
// matches the `params:` idiom escurel already uses on query pages and is the
// only form that preserves the author's field order.
const CHURN_SKILL: &str = "\
---
type: skill
id: churn-report
description: Churn over a window.
required_frontmatter: [at, generated_by]
optional_frontmatter: [note]
params:
  - {name: window, kind: string, required: true, label: Window, description: 'e.g. 30d'}
  - {name: grouping, kind: string}
  - {name: depth, kind: integer, required: false}
  - {name: include_churned, kind: boolean, required: true}
---
# churn-report
";

// The same idea in the MAP form the issue's `## Possible shape` block wrote.
// An author copying the issue verbatim must not get silence.
const COMPARE_SKILL: &str = "\
---
type: skill
id: compare
description: Compare two instances.
params:
  left:  {kind: string, required: true, description: 'first instance'}
  right: {kind: string, required: true}
---
# compare
";

// Aliases and sloppy casing. `text`/`int`/`bool` are the spellings an author
// arrives with from the query-page `params:` idiom (`type: text`).
const ALIASED_SKILL: &str = "\
---
type: skill
id: aliased
description: Alias soup.
params:
  - {name: a, kind: text}
  - {name: b, kind: '  INT  '}
  - {name: c, kind: Bool}
  - {name: d, type: string, required: true}
---
# aliased
";

// The mis-declaration. `kind: dat` is a typo for `date`, which is not in the
// renderable set anyway. Seeded so the catalogue behaviour is pinned
// independently of whatever `validate` says about it.
const BROKEN_SKILL: &str = "\
---
type: skill
id: broken
description: Fat fingers.
params:
  - {name: since, kind: dat, required: true}
  - {name: shape, required: true}
---
# broken
";

fn catalogue() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", NOTE_SKILL)
        .skill("churn-report", CHURN_SKILL)
        .skill("compare", COMPARE_SKILL)
        .skill("aliased", ALIASED_SKILL)
        .skill("broken", BROKEN_SKILL)
        .done()
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(catalogue()),
        config_overrides: ConfigOverrides::default(),
    })
    .await
}

/// Call a tool over MCP-over-HTTP, returning `structuredContent`.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

async fn skills_by_id(p: &EscurelProcess, token: &str) -> std::collections::HashMap<String, Value> {
    let out = call(p, token, "list_skills", json!({})).await;
    out["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|s| (s["id"].as_str().unwrap().to_owned(), s.clone()))
        .collect()
}

fn params_of(skill: &Value) -> &Vec<Value> {
    skill["params"]
        .as_array()
        .unwrap_or_else(|| panic!("no params on {skill}"))
}

// --- list_skills ------------------------------------------------------

/// The acceptance criterion: a skill declares invocation parameters, and
/// `list_skills` surfaces them — enough to build a form, in one call, with no
/// `expand`.
#[tokio::test]
async fn list_skills_surfaces_declared_invocation_params() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;

    let params = params_of(&by_id["churn-report"]);
    assert_eq!(params.len(), 4, "one entry per declared param: {params:?}");

    // Authored order is preserved: a generated form renders the fields in
    // the order the author put them on the page, not alphabetically.
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["window", "grouping", "depth", "include_churned"]
    );

    // Every field an A2UI `form` needs: {name, label, kind, required}.
    assert_eq!(params[0]["name"], json!("window"));
    assert_eq!(params[0]["kind"], json!("string"));
    assert_eq!(params[0]["required"], json!(true));
    assert_eq!(params[0]["label"], json!("Window"));
    assert_eq!(params[0]["description"], json!("e.g. 30d"));

    // `required:` omitted means optional — declaring a param is not the same
    // as demanding it.
    assert_eq!(params[1]["name"], json!("grouping"));
    assert_eq!(params[1]["required"], json!(false));
    // ...and an omitted label/description is omitted, not an empty string:
    // the client falls back to the name rather than rendering a blank caption.
    assert_eq!(params[1].get("label"), None);
    assert_eq!(params[1].get("description"), None);

    // The other two renderable kinds, so a passing `string` assertion above
    // cannot be a server that hardcodes one kind.
    assert_eq!(params[2]["kind"], json!("integer"));
    assert_eq!(params[2]["required"], json!(false), "explicit false");
    assert_eq!(params[3]["kind"], json!("boolean"));
    assert_eq!(params[3]["required"], json!(true));
}

/// The whole point of CR-7: the run inputs are not the instance schema. A
/// client reading `params` must get the declared parameters, never a
/// re-projection of `required_frontmatter`.
#[tokio::test]
async fn params_are_independent_of_required_frontmatter() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;
    let churn = &by_id["churn-report"];

    // Positive control: the instance schema is still reported, unchanged.
    assert_eq!(churn["required_frontmatter"], json!(["at", "generated_by"]));
    assert_eq!(churn["optional_frontmatter"], json!(["note"]));

    let names: std::collections::HashSet<&str> = params_of(churn)
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        names.is_disjoint(&["at", "generated_by", "note"].into_iter().collect()),
        "params must not be derived from the instance schema: {names:?}"
    );
    assert!(names.contains("window"));
}

/// The map form the issue's `## Possible shape` block wrote. Accepted, with
/// the key as the param name.
#[tokio::test]
async fn the_mapping_form_is_accepted_with_the_key_as_the_name() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;

    let params = params_of(&by_id["compare"]);
    assert_eq!(params.len(), 2, "{params:?}");
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["left", "right"], "deterministic order");
    assert_eq!(params[0]["kind"], json!("string"));
    assert_eq!(params[0]["required"], json!(true));
    assert_eq!(params[0]["description"], json!("first instance"));
    assert_eq!(params[1]["required"], json!(true));
    assert_eq!(params[1].get("description"), None);
}

/// Kinds are normalised to the three A2UI-renderable values, so the client
/// needs no mapping layer. `type:` is accepted as a synonym for `kind:`
/// because that is the spelling the query-page `params:` idiom uses.
#[tokio::test]
async fn kinds_are_normalised_to_the_renderable_set() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;

    let params = params_of(&by_id["aliased"]);
    assert_eq!(params[0]["kind"], json!("string"), "text -> string");
    assert_eq!(params[1]["kind"], json!("integer"), "'  INT  ' -> integer");
    assert_eq!(params[2]["kind"], json!("boolean"), "Bool -> boolean");
    assert_eq!(params[3]["kind"], json!("string"), "`type:` is a synonym");
    assert_eq!(params[3]["required"], json!(true), "...and reads siblings");
}

/// The direction-of-failure assertion this key exists for. A param whose
/// `kind:` cannot be read — a typo, or a type outside the renderable set —
/// is still reported, as `string`. Dropping it would delete a REQUIRED field
/// from a generated form; the caller would then omit it and the run would
/// fail with nothing on the page to explain why.
#[tokio::test]
async fn an_unreadable_kind_degrades_to_string_and_never_vanishes() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;

    let params = params_of(&by_id["broken"]);
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["since", "shape"],
        "a mis-declared kind must not delete the param: {params:?}"
    );

    // `kind: dat` — unreadable, so the safest renderable kind.
    assert_eq!(params[0]["kind"], json!("string"));
    // ...and `required:` still lands, so the form still demands the field.
    assert_eq!(
        params[0]["required"],
        json!(true),
        "the requiredness survives the bad kind"
    );

    // No `kind:` at all is the same case: declaring a parameter without
    // saying what it holds is a text box, not nothing.
    assert_eq!(params[1]["kind"], json!("string"));
    assert_eq!(params[1]["required"], json!(true));

    // Positive control against a server that just answers "string": the
    // well-formed integer/boolean above did not become strings.
    assert_eq!(
        params_of(&by_id["churn-report"])[2]["kind"],
        json!("integer")
    );
}

/// Backward compatibility, which is load-bearing: every skill page that
/// exists today declares no `params:`. Its row must be what it was, and the
/// key must be ABSENT rather than an empty array — an old client that round-
/// trips a skill row is not handed a field it never sent.
#[tokio::test]
async fn a_skill_without_params_keeps_its_whole_row_and_grows_no_key() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let by_id = skills_by_id(&p, &token).await;
    let note = &by_id["note"];

    assert_eq!(note.get("params"), None, "no key at all: {note}");

    assert_eq!(note["description"], json!("A note."));
    assert_eq!(note["required_frontmatter"], json!([]));
    assert_eq!(note["optional_frontmatter"], json!([]));
    assert_eq!(note["is_event_typed"], json!(false));
    assert_eq!(note["visibility"], json!("public"));
    assert_eq!(note["layer"], json!("overlay"));
    assert_eq!(note["backend"]["kind"], json!("markdown"));
    assert_eq!(note["capabilities"]["writable"], json!(true));
    assert_eq!(note.get("autonomy"), None);

    // The shipped meta-skill page declares no params either — proof the
    // absence above is not an artefact of this test's own fixture.
    assert_eq!(by_id["escurel"].get("params"), None);
}

/// `expand` is the RAW page and stays raw: the catalogue normalises, the
/// page does not. A curator fixing a flagged declaration sees the bytes they
/// wrote, not a guess.
#[tokio::test]
async fn expand_reports_the_declared_params_verbatim() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let out = call(
        &p,
        &token,
        "expand",
        json!({ "page_id": "markdown/skills/broken.md" }),
    )
    .await;
    assert_eq!(out["frontmatter"]["params"][0]["kind"], json!("dat"));
    assert_eq!(out["frontmatter"]["params"][1].get("kind"), None);

    // Positive control: a well-formed page is passed through unchanged too,
    // aliases included — `list_skills` normalises, `expand` does not.
    let out = call(
        &p,
        &token,
        "expand",
        json!({ "page_id": "markdown/skills/aliased.md" }),
    )
    .await;
    assert_eq!(out["frontmatter"]["params"][0]["kind"], json!("text"));
    assert_eq!(out["frontmatter"]["params"][1]["kind"], json!("  INT  "));
}

// --- validate ---------------------------------------------------------

fn issues_with(v: &Value, code: &str) -> Vec<Value> {
    v["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|i| i["code"] == code)
        .cloned()
        .collect()
}

/// An unreadable `kind:` is a WARNING, not an error. The page still works —
/// the param is reported as `string` — so refusing the write would be a
/// behaviour change for a key that has never been validated, while saying
/// nothing would leave the author's typo invisible. Contrast `autonomy:`,
/// where the mis-declaration is error-severity because there the failure mode
/// is an ungated write.
#[tokio::test]
async fn validate_warns_on_an_unreadable_param_kind_only() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Positive controls: the renderable set and its aliases stay silent...
    for good in ["string", "integer", "boolean", "text", "int", "bool", "INT"] {
        let page = format!(
            "---\ntype: skill\nid: note\ndescription: d.\n\
             params:\n  - {{name: a, kind: {good}}}\n---\n# note\n"
        );
        let out = call(&p, &token, "validate", json!({ "content": page })).await;
        assert!(
            issues_with(&out, "frontmatter_param_kind_unknown").is_empty(),
            "`{good}` should be accepted: {out}"
        );
        assert_eq!(out["ok"], json!(true), "`{good}`: {out}");
    }
    // ...as does a page with no `params:` at all.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": NOTE_SKILL.to_owned() }),
    )
    .await;
    assert!(issues_with(&out, "frontmatter_param_kind_unknown").is_empty());
    assert_eq!(out["ok"], json!(true));

    // The negatives. `date`/`number` are real types that simply are not in
    // the A2UI-renderable set; `dat`/`strng` are typos; `3` is not a string.
    for bad in ["dat", "strng", "date", "number", "3"] {
        let page = format!(
            "---\ntype: skill\nid: note\ndescription: d.\n\
             params:\n  - {{name: a, kind: {bad}}}\n---\n# note\n"
        );
        let out = call(&p, &token, "validate", json!({ "content": page })).await;
        let found = issues_with(&out, "frontmatter_param_kind_unknown");
        assert_eq!(found.len(), 1, "`{bad}` should be flagged once: {out}");
        assert_eq!(found[0]["severity"], json!("warning"));
        assert_eq!(found[0]["location"], json!("frontmatter.params.a.kind"));
        assert!(
            found[0]["suggestion"]
                .as_str()
                .unwrap_or_default()
                .contains("boolean"),
            "the suggestion names the renderable set: {found:?}"
        );
        // A warning does not fail the draft: the page is still writable.
        assert_eq!(out["ok"], json!(true), "`{bad}` must not fail validation");
    }
}

/// A `params:` block that is neither a list nor a mapping, or an entry with
/// no name, cannot be turned into a form field at all — that is an error,
/// because unlike a bad kind there is nothing to degrade to.
#[tokio::test]
async fn validate_errors_on_a_params_block_it_cannot_read() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    for bad in [
        "params: 3\n",
        "params: 'window'\n",
        "params:\n  - {kind: string}\n",
    ] {
        let page = format!("---\ntype: skill\nid: note\ndescription: d.\n{bad}---\n# note\n");
        let out = call(&p, &token, "validate", json!({ "content": page })).await;
        let found = issues_with(&out, "frontmatter_params_malformed");
        assert_eq!(found.len(), 1, "should be flagged: {out}");
        assert_eq!(found[0]["severity"], json!("error"));
        assert_eq!(out["ok"], json!(false), "{out}");
    }

    // Positive controls: both well-formed shapes pass.
    for good in [
        "params:\n  - {name: a, kind: string}\n",
        "params:\n  a: {kind: string}\n",
        "params: []\n",
    ] {
        let page = format!("---\ntype: skill\nid: note\ndescription: d.\n{good}---\n# note\n");
        let out = call(&p, &token, "validate", json!({ "content": page })).await;
        assert!(
            issues_with(&out, "frontmatter_params_malformed").is_empty(),
            "{good} should pass: {out}"
        );
        assert_eq!(out["ok"], json!(true), "{out}");
    }
}

/// `params:` is a SKILL-page key. On an INSTANCE page it is already taken —
/// a `[[query::*]]` page declares `params:` with a `type:` drawn from a
/// different vocabulary (`date`, `number`) and binds them as SQL parameters.
/// That surface must not start emitting findings.
#[tokio::test]
async fn validate_ignores_params_on_a_query_instance_page() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let query_page = "\
---
type: instance
skill: note
id: churn-trend
params:
  - {name: from_date, type: date, required: true}
  - {name: floor, type: number}
---
# churn-trend
";
    let out = call(&p, &token, "validate", json!({ "content": query_page })).await;
    assert!(
        issues_with(&out, "frontmatter_param_kind_unknown").is_empty(),
        "instance pages are out of scope: {out}"
    );
    assert!(issues_with(&out, "frontmatter_params_malformed").is_empty());
    assert_eq!(out["ok"], json!(true), "{out}");

    // Positive control in the same test: the identical block on a SKILL page
    // IS flagged, so the silence above is scoping and not a dead check.
    let skill_page = query_page.replace("type: instance\nskill: note", "type: skill");
    let out = call(&p, &token, "validate", json!({ "content": skill_page })).await;
    assert_eq!(
        issues_with(&out, "frontmatter_param_kind_unknown").len(),
        2,
        "{out}"
    );
}
