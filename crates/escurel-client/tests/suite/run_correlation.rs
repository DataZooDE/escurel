//! Every call a run makes must be joinable to that run from the gateway side.
//!
//! The runner minted its own trace id into event provenance and the client
//! sent no correlation header at all, so a gateway log line and a runner log
//! line shared no identifier. "A run went missing — where is it?" therefore
//! had no query behind it: you could see the run in the ledger, and you could
//! see tool calls in the gateway log, and nothing tied the two together.
//!
//! Asserted against a raw socket rather than a real gateway on purpose: what
//! is being pinned is what goes ON THE WIRE. A test that drove a gateway
//! would pass just as happily if the headers were dropped and the gateway
//! invented its own id, which is the state this fixes.

use escurel_client::Client;
use escurel_client::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Accept one request, return its raw head, and answer with a valid
/// JSON-RPC `tools/call` result so the client's own parsing succeeds.
async fn record_one_request(listener: tokio::net::TcpListener) -> String {
    let (mut sock, _) = listener.accept().await.expect("accept");
    let mut buf = vec![0u8; 8192];
    let n = sock.read(&mut buf).await.expect("read");
    let head = String::from_utf8_lossy(&buf[..n]).to_string();

    let body = br#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"skills":[]}}}"#;
    let resp = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await.expect("write head");
    sock.write_all(body).await.expect("write body");
    sock.flush().await.expect("flush");
    head
}

#[tokio::test]
async fn a_tagged_client_sends_the_run_id_on_the_wire() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let recorder = tokio::spawn(record_one_request(listener));

    let client = Client::connect(&format!("http://{addr}"), SecretString::from(String::new()))
        .await
        .expect("connect")
        .with_run_id("01RUNIDEXAMPLE0000000000AB");

    let _ = client.list_skills(Default::default()).await;

    let head = recorder.await.expect("recorder");
    let lower = head.to_lowercase();
    assert!(
        lower.contains("x-escurel-run-id: 01runidexample0000000000ab"),
        "the run id must be on the wire: {head}"
    );
    assert!(
        lower.contains("x-request-id: 01runidexample0000000000ab."),
        "the request id must be the run id plus a sequence, so one call is \
         individually addressable while the run stays greppable: {head}"
    );
}

/// An untagged client — the CLI, the TUI, an application backend — must send
/// neither header, so the gateway keeps minting its own request id.
#[tokio::test]
async fn an_untagged_client_sends_no_correlation_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let recorder = tokio::spawn(record_one_request(listener));

    let client = Client::connect(&format!("http://{addr}"), SecretString::from(String::new()))
        .await
        .expect("connect");
    let _ = client.list_skills(Default::default()).await;

    let head = recorder.await.expect("recorder").to_lowercase();
    assert!(
        !head.contains("x-escurel-run-id"),
        "a client that is not acting for a run must not claim to be: {head}"
    );
}
