/// Auto-refresh: keep the explorer's read views current without a manual
/// reload (F5). All read data is served by `FutureProvider`s, which are
/// pull-once + cache; a periodic timer invalidates them so they re-fetch.
///
/// On by default — the operator browses a live knowledge base. A toggle +
/// interval let an operator pause it (e.g. while reading) or tune the
/// cadence. This is the pragmatic v1; an event-driven path over Escurel's
/// `/ws` (the client's `awareness()` stream) can replace the poll later.
library;

import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import 'crm_providers.dart';

/// Whether the explorer polls for changes. Default `true`.
final autoRefreshEnabledProvider = StateProvider<bool>((ref) => true);

/// How often to re-fetch when [autoRefreshEnabledProvider] is on.
final autoRefreshIntervalProvider = StateProvider<Duration>(
  (ref) => const Duration(seconds: 15),
);

/// Invalidate the explorer's read providers so they re-fetch from the
/// backend. Pure cache-busting — the providers that something is watching
/// re-resolve; idle ones stay lazy. Safe to call repeatedly.
void refreshExplorerData(WidgetRef ref) {
  ref.invalidate(allInstancesRawProvider);
  ref.invalidate(skillsCatalogueProvider);
  ref.invalidate(currentPageProvider);
  ref.invalidate(currentBacklinksProvider);
  ref.invalidate(currentOutgoingLinksProvider);
  ref.invalidate(entityEventHistoryProvider);
  ref.invalidate(inboxEventsProvider);
  ref.invalidate(instanceSnapshotsProvider);
}

/// Drives [refreshExplorerData] on a timer while mounted. Invisible — wraps
/// the workspace and re-arms whenever the enabled flag or interval changes.
class AutoRefresher extends ConsumerStatefulWidget {
  const AutoRefresher({super.key, required this.child});
  final Widget child;

  @override
  ConsumerState<AutoRefresher> createState() => _AutoRefresherState();
}

class _AutoRefresherState extends ConsumerState<AutoRefresher> {
  Timer? _timer;

  void _reconfigure() {
    _timer?.cancel();
    _timer = null;
    if (!ref.read(autoRefreshEnabledProvider)) return;
    final interval = ref.read(autoRefreshIntervalProvider);
    _timer = Timer.periodic(interval, (_) {
      if (mounted) refreshExplorerData(ref);
    });
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _reconfigure();
    });
  }

  @override
  Widget build(BuildContext context) {
    // Re-arm the timer whenever the operator toggles polling or changes the
    // cadence.
    ref.listen(autoRefreshEnabledProvider, (_, _) => _reconfigure());
    ref.listen(autoRefreshIntervalProvider, (_, _) => _reconfigure());
    return widget.child;
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }
}
