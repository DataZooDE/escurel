/// Run usage + cost (`usage_report`) for the explorer's badges and the
/// Agents panel. `usage_report` is admin-only, so every read here asks
/// `whoami` first and yields `null` for a non-admin — the caller hides
/// the figure instead of showing a refusal.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import 'providers.dart';

/// Who the gateway takes this client for — cached for the session.
final whoamiProvider = FutureProvider<Whoami>(
  (ref) => ref.watch(escurelClientProvider).whoami(),
);

/// The window every usage figure in the explorer covers.
const usageWindow = Duration(days: 30);

/// The short label a figure over [usageWindow] carries.
const usageWindowLabel = '30d';

/// A cost as the explorer prints it: `$0.42`.
String formatUsd(double usd) => '\$${usd.toStringAsFixed(2)}';

/// "7 runs" / "1 run".
String formatRuns(int n) => '$n ${n == 1 ? 'run' : 'runs'}';

/// The last [usageWindow] of `usage_report`, keyed by the value of the one
/// grouping dimension named (`page`, `agent`, …); `null` when the caller is
/// not an admin.
final usageByProvider = FutureProvider.family<Map<String, UsageRow>?, String>((
  ref,
  dim,
) async {
  final me = await ref.watch(whoamiProvider.future);
  if (!me.isAdmin) return null;
  final since = DateTime.now().toUtc().subtract(usageWindow);
  final report = await ref
      .watch(escurelClientProvider)
      .usageReport(groupBy: [dim], since: since.toIso8601String());
  return report.by(dim);
});
