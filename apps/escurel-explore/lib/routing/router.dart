/// go_router configuration for the explorer.
///
/// URL scheme:
/// - `/`               — editor surface, no page focused
/// - `/p/:pageId`      — editor surface focused on a specific page
/// - `/inspector`      — inspector drawer at the default (md) panel
/// - `/inspector/:id`  — inspector drawer on a specific panel
///
/// Tenant-scoped variants land when multi-tenant support arrives
/// (likely concurrent with M3+ admin tools). The current scheme is
/// deliberately tenant-implicit — fixture mode and the first HTTP
/// deployment both operate against a single tenant.
library;

import 'package:escurel_explorer_kit/escurel_explorer_kit.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

/// The route table. Extracted as a top-level constant so test
/// overrides can build a fresh [GoRouter] with a different
/// `initialLocation` without depending on the [routerProvider].
final List<RouteBase> appRoutes = [
  GoRoute(path: '/', builder: (context, state) => const AppShell()),
  GoRoute(
    path: '/p/:pageId',
    builder: (context, state) => _OpenPageEffect(
      pageId: state.pathParameters['pageId']!,
      child: const AppShell(),
    ),
  ),
  GoRoute(
    path: '/inspector',
    builder: (context, state) => const InspectorShell(panelId: 'md'),
    routes: [
      GoRoute(
        path: ':panelId',
        builder: (context, state) =>
            InspectorShell(panelId: state.pathParameters['panelId'] ?? 'md'),
      ),
    ],
  ),
];

final routerProvider = Provider<GoRouter>((ref) {
  return GoRouter(initialLocation: '/', routes: appRoutes);
});

/// Side-effect widget that sets the current page id from the URL
/// on first build, then becomes a transparent pass-through. Used
/// for deep links like `/p/customer__acme`.
class _OpenPageEffect extends ConsumerStatefulWidget {
  const _OpenPageEffect({required this.pageId, required this.child});

  final String pageId;
  final Widget child;

  @override
  ConsumerState<_OpenPageEffect> createState() => _OpenPageEffectState();
}

class _OpenPageEffectState extends ConsumerState<_OpenPageEffect> {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) {
        ref.read(currentPageIdProvider.notifier).state = widget.pageId;
      }
    });
  }

  @override
  Widget build(BuildContext context) => widget.child;
}

/// Used by [EscurelExploreApp] to wire `MaterialApp.router`. Kept
/// in this file so router config stays in one place.
extension AppRouter on Ref {
  GoRouter get router => read(routerProvider);
}
