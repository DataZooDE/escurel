//! End-to-end tests for `escurel-tui` against a real gateway.
//!
//! No mocks: each test spawns a real `EscurelProcess`, seeds the canonical
//! customer/acme/initech corpus, mints an Agent token, drives [`DataSource`] +
//! [`App`] state transitions directly (no TTY), renders to a [`TestBackend`]
//! after each step, and asserts the rendered text. Mirrors the spawn/seed
//! pattern in `crates/escurel-cli/tests/cli_e2e.rs`.

use escurel_client::{AssignEventRequest, CaptureEventRequest, SecretString};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use escurel_tui::{App, DataRequest, DataSource, Screen};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

const ACME_PAGE_ID: &str = "markdown/instances/customer/acme.md";

async fn spawn_seeded() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides::default(),
    })
    .await
}

async fn connect(process: &EscurelProcess) -> DataSource {
    let endpoint = process.grpc_endpoint().expect("grpc endpoint").to_owned();
    let token = SecretString::from(process.mint_token(TENANT, Role::Agent));
    DataSource::connect(&endpoint, token)
        .await
        .expect("connect data source")
}

/// Render `app` to a 120x40 `TestBackend` and return the buffer as a string.
fn render_to_string(app: &App) -> String {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal.draw(|f| app.render(f)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let area = *buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Load `req` through the data source into `app`, panicking on RPC error.
async fn load(source: &DataSource, app: &mut App, req: DataRequest) {
    let data = source.fetch(&req).await.expect("fetch");
    app.set_data(data);
}

fn enter() -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter)
}

/// Render the screen `screen` of `source` after loading it, returning the text.
async fn render_screen(source: &DataSource, screen: Screen) -> String {
    let mut app = App::with_screen(screen);
    let req = app.current_request();
    load(source, &mut app, req).await;
    render_to_string(&app)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigation_flow_renders_corpus() {
    let process = spawn_seeded().await;
    let source = connect(&process).await;

    let mut app = App::new();

    // 1. Skills screen -> contains "customer".
    let req = app.current_request();
    load(&source, &mut app, req).await;
    let skills_view = render_to_string(&app);
    assert!(
        skills_view.contains("customer"),
        "skills view should list the customer skill:\n{skills_view}"
    );

    // 2. Drill into the customer skill -> instances contains "acme".
    assert_eq!(app.current(), &Screen::Skills);
    let req = app
        .on_key(enter())
        .expect("drill into customer should request instances");
    assert_eq!(req, DataRequest::Instances("customer".to_string()));
    load(&source, &mut app, req).await;
    let instances_view = render_to_string(&app);
    assert!(
        instances_view.contains("acme"),
        "instances view should list acme:\n{instances_view}"
    );

    // 3. Drill into the acme instance -> entity shows "Acme" and the outgoing
    //    wikilink to initech (the body links [[customer::initech]]).
    let req = app
        .on_key(enter())
        .expect("drill into acme should request entity");
    assert!(matches!(req, DataRequest::Entity(_)));
    load(&source, &mut app, req).await;
    let entity_view = render_to_string(&app);
    assert!(
        entity_view.contains("Acme"),
        "entity view should show the Acme title/body:\n{entity_view}"
    );
    assert!(
        entity_view.contains("initech"),
        "entity view should show the outgoing link to initech:\n{entity_view}"
    );

    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_then_assign_renders_event_in_events() {
    let process = spawn_seeded().await;
    let source = connect(&process).await;

    // Capture an event via the real client. A captured event lands in the
    // global inbox (status "inbox") until it is explicitly assigned.
    let stored = source
        .client()
        .capture_event(CaptureEventRequest {
            event_id: String::new(),
            at: String::new(),
            source: "manual".to_string(),
            mime: "text/plain".to_string(),
            label_skill: "note".to_string(),
            instance_page_id: String::new(),
            title: "Renewal call".to_string(),
            body: "Discussed contract renewal.".to_string(),
            provenance_json: String::new(),
        })
        .await
        .expect("capture event");

    // Inbox screen render shows the captured event title.
    let inbox_view = render_screen(&source, Screen::Inbox).await;
    assert!(
        inbox_view.contains("Renewal call"),
        "inbox view should show the captured event title:\n{inbox_view}"
    );

    // Assign the event to the acme instance -> it becomes "processed" and
    // surfaces in that instance's Events history.
    source
        .client()
        .assign_event(AssignEventRequest {
            event_id: stored.event_id.clone(),
            instance_page_id: ACME_PAGE_ID.to_string(),
        })
        .await
        .expect("assign event");

    let events_view = render_screen(
        &source,
        Screen::Events {
            instance: ACME_PAGE_ID.to_string(),
        },
    )
    .await;
    assert!(
        events_view.contains("Renewal call"),
        "events view should show the assigned event title:\n{events_view}"
    );

    process.shutdown().await;
}
