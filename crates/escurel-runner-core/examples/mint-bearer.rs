//! Mint an escurel bearer from the platform signing key, for operator use.
//!
//! An EXAMPLE and not a binary on purpose: this is a key-handling tool, and
//! shipping it inside the runner image would put a token minter next to the
//! credential it mints from. `cargo run --example` keeps it on a developer's
//! machine, where the key already has to be to run it at all.
//!
//! It exists because the `kid` derivation is not obvious — the gateway
//! verifies against a JWKS that names the key `agent-escurel-<fingerprint>`,
//! and a hand-rolled token with a bare fingerprint is rejected on a `kid`
//! miss. Reusing `Signer` means the one correct derivation has one
//! implementation.
//!
//! The key arrives on STDIN so it never reaches a process argument, an
//! environment listing, or a shell history file:
//!
//! ```text
//! gcloud secrets versions access latest \
//!   --secret=agent-template-escurel-signing-key \
//!   --project=hetzner-agent-backplane \
//!   | cargo run -q -p escurel-runner-core --example mint-bearer -- \
//!       --tenant datazoo-loops --subject operator:seed
//! ```
//!
//! The minted token carries `roles: ["escurel:admin"]` — see `Signer::mint`.
//! Treat it as the credential it is: it is short-lived, but while it lives it
//! can write anything in that tenant.

use std::io::Read as _;

use escurel_runner_core::Signer;

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

fn main() {
    let issuer = arg("--issuer", "https://agent-lab.data-zoo.de");
    let audience = arg("--audience", "escurel");
    let tenant = arg("--tenant", "datazoo-loops");
    let subject = arg("--subject", "operator:seed");
    let ttl: u64 = arg("--ttl-secs", "900").parse().unwrap_or(900);

    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .expect("read signing key from stdin");
    if key.trim().is_empty() {
        eprintln!("error: no signing key on stdin");
        std::process::exit(2);
    }

    let signer = Signer::build(issuer, audience, tenant, None, &key)
        .unwrap_or_else(|e| panic!("build signer: {e}"));
    let token = signer
        .mint(&subject, ttl)
        .unwrap_or_else(|e| panic!("mint: {e}"));
    println!("{token}");
}
