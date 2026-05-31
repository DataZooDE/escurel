#!/usr/bin/env bash
# verify-tui.sh — end-to-end smoke test for the escurel-tui terminal UI.
#
# The TUI's navigation + render logic is exercised against a real gateway
# (spawned by escurel-test-support, no mocks) and drawn to a ratatui
# `TestBackend`, so the whole surface is verifiable headlessly — there is
# no TTY to drive. This script just runs that suite and is exit-code
# gated, the CLI/TUI analogue of scripts/verify-demo.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> running escurel-tui end-to-end suite (real gateway, TestBackend)"
cargo test -p escurel-tui

echo "==> escurel-tui verification passed"
