# Explorer auth — OIDC PKCE deferred until real issuer

**Date:** 2026-05-24.
**Status:** Deferred to a follow-up PR.
**Scope:** `apps/escurel-explore/lib/client/http_escurel_client.dart`
and the `ESCUREL_EXPLORE_AUTH` env var.

## What we have today

Three modes, dispatched by `--dart-define=ESCUREL_EXPLORE_AUTH=…`:

- **`none`** (default) — no `Authorization` header. Works for
  tailnet-only deployments where the substrate's tailnet ACL is the
  auth boundary. This was the v0 deployment shape. (Historical note:
  the original Nomad jobspec it referenced was removed in the
  Kamal-substrate migration; internal/tailnet exposure is now declared
  in the substrate repo's `apps/registry.yml` — see `docs/deploy/substrate.md`.)
- **`bearer`** — read a static token from
  `--dart-define=ESCUREL_EXPLORE_TOKEN=…` and ride it as
  `Authorization: Bearer <token>`. Used by the rare dev who wants to
  point at a staging escurel-server with auth turned on.
- **`oidc`** — currently not implemented. The intent is OIDC
  Authorization Code + PKCE against the substrate's
  Dex / Keycloak issuer, refresh handled silently via the
  iframe pattern, JWT carried on dio requests just like bearer.

## Why we deferred OIDC

When the explorer PR-6a (HttpEscurelClient) landed, no OIDC issuer
was reachable from a dev laptop, the substrate-platform skill's
auth conventions had not yet hardened (Dex vs Keycloak choice was
still open), and the v0 deployment is tailnet-only so auth is
unnecessary anyway. Implementing OIDC against a hypothetical issuer
would have meant inventing the discovery URL, the client_id
convention, and the post-logout redirect — all of which the
substrate will pin in its own PR.

## When to implement

Add OIDC when ONE of these is true:

1. The substrate skill commits to a specific issuer (Dex or
   Keycloak) with a stable discovery URL and a per-app client
   registration convention.
2. The explorer needs to ride a Fabio public ingress (drop the
   tailnet-only restriction).
3. A real user reports "I want to share a deep link to a page" and
   the linked page lives behind tenant ACLs that the tailnet alone
   can't differentiate.

## How to implement (sketch, not committed code)

- Add `package:openid_client` or `package:flutter_appauth` to
  `apps/escurel-explore/pubspec.yaml`.
- New `lib/auth/oidc_session.dart` — runs PKCE on first load,
  caches token + refresh in `flutter_secure_storage`, exposes a
  `tokenProvider` (Riverpod) the dio interceptor watches.
- Update `lib/client/http_escurel_client.dart` to read the token
  from `tokenProvider` per request (replacing the constructor-time
  static `bearerToken` parameter).
- Add a topbar "logged in as…" chip + sign-out menu.

## How to recognise the symptom

If a future contributor opens
`lib/client/http_escurel_client.dart` and sees the comment
referencing this note, they're at the deferred decision point.
Read this note, check the three "When to implement" triggers, and
decide whether the OIDC PR is now in scope.
