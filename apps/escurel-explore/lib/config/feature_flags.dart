/// Feature-flag gating derived from the backend's reported
/// [BackendCapability] set.
///
/// The escurel-server publishes its capabilities via `/version`.
/// The explorer queries that once on startup, caches the set in
/// [currentCapabilitiesProvider], and exposes typed boolean
/// providers that widgets watch.
///
/// The rule: **never hide a feature whose backend capability we
/// don't know about.** A missing `agentWriteTools` capability is a
/// fact; *not knowing* whether the backend has it is uncertain.
/// Fixture mode reports a full `agentReadTools`-only set so the
/// gating behaves identically against fixtures and an early-M3
/// server.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';

/// Snapshot of the backend's reported capability set + identity.
/// Refreshes when the editor reconnects (not implemented today —
/// PR-6c lands a polling/reconnect strategy when a real server is
/// available to verify against).
final currentCapabilitiesProvider = FutureProvider<VersionInfo>((ref) {
  return ref.watch(escurelClientProvider).version();
});

/// Read tools (search, resolve, expand, neighbours, list_*,
/// run_stored_query) — the editor's catalogue + entity editor +
/// backlinks pane all require this.
final readEnabledProvider = Provider<bool>((ref) {
  final async = ref.watch(currentCapabilitiesProvider);
  return async.maybeWhen(
    data: (v) => v.capabilities.contains(BackendCapability.agentReadTools),
    orElse: () => false,
  );
});

/// Write tools (validate, update_page). Controls the write surface
/// at the bottom of the editor.
final writeEnabledProvider = Provider<bool>((ref) {
  final async = ref.watch(currentCapabilitiesProvider);
  return async.maybeWhen(
    data: (v) => v.capabilities.contains(BackendCapability.agentWriteTools),
    orElse: () => false,
  );
});

/// Live mode (open/apply/close session over WS). Controls the
/// co-edit toggle on the editor toolbar.
final liveEnabledProvider = Provider<bool>((ref) {
  final async = ref.watch(currentCapabilitiesProvider);
  return async.maybeWhen(
    data: (v) => v.capabilities.contains(BackendCapability.liveSession),
    orElse: () => false,
  );
});

/// Admin MCP tools (admin_list_lanes, admin_lane_keys,
/// admin_lane_blob, admin_index_query). Controls the LaneStore and
/// Index inspector panels in the Dev Inspector drawer.
final adminEnabledProvider = Provider<bool>((ref) {
  final async = ref.watch(currentCapabilitiesProvider);
  return async.maybeWhen(
    data: (v) => v.capabilities.contains(BackendCapability.adminTools),
    orElse: () => false,
  );
});

/// Stored-query support (run_stored_query). Most servers will report
/// this alongside the read tools, but it's a separate capability so
/// the editor can disable the query panel when the backend's
/// query catalogue is empty or the feature is opted out.
final storedQueriesEnabledProvider = Provider<bool>((ref) {
  final async = ref.watch(currentCapabilitiesProvider);
  return async.maybeWhen(
    data: (v) => v.capabilities.contains(BackendCapability.storedQueries),
    orElse: () => false,
  );
});
