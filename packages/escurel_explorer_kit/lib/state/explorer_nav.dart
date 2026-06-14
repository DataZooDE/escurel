import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

/// Navigation seam between the explorer shell and its host.
///
/// The standalone escurel-explore app leaves [explorerNavigateProvider]
/// null, so the shell drives go_router directly. An embedding host that
/// has no go_router (e.g. an operator dashboard) injects a handler and
/// sets [explorerEmbeddedProvider] true to hide the standalone-only
/// chrome (the CRM / dev-inspector links), keeping the shell from ever
/// touching a router that isn't there.
typedef ExplorerNavigate = void Function(String path);

/// Host-supplied navigation handler. `null` → fall back to go_router.
final explorerNavigateProvider = Provider<ExplorerNavigate?>((ref) => null);

/// True when the shell is embedded in a host without go_router, so the
/// standalone-only top-level surfaces (CRM, inspector) are hidden.
final explorerEmbeddedProvider = Provider<bool>((ref) => false);

/// Navigate to [path] via the injected handler if present, else
/// go_router. The single choke-point every shell navigation goes
/// through, so the kit never hard-codes a router.
void explorerGo(BuildContext context, WidgetRef ref, String path) {
  final nav = ref.read(explorerNavigateProvider);
  if (nav != null) {
    nav(path);
  } else {
    GoRouter.of(context).go(path);
  }
}
