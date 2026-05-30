import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/escurel_client.dart';
import '../client/models.dart';
import '../md/wikilink.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

/// Clickable pill rendering a `[[skill::id]]` reference.
///
/// Resolves via [EscurelClient.resolve] on first build; the pill's
/// outline turns red when the link is dangling and grey otherwise.
/// Tapping a resolvable pill navigates the editor to the linked
/// page through [currentPageIdProvider].
class WikilinkPill extends ConsumerWidget {
  const WikilinkPill({super.key, required this.ref});

  final WikilinkRef ref;

  @override
  Widget build(BuildContext context, WidgetRef wref) {
    final markup = ref.toMarkup();
    final resolved = wref.watch(_resolvedRefProvider(markup));
    final text = ref.alias ?? (ref.skill != null ? '${ref.skill}::${ref.id}' : ref.id ?? '?');

    return resolved.when(
      loading: () => _pillBody(context, text, kOutlineVariant, kOnSurfaceVariant, onTap: null),
      error: (e, _) => _pillBody(context, text, kError, kError, onTap: null),
      data: (r) {
        final colour = r.exists ? kPrimary : kError;
        return _pillBody(
          context,
          text,
          colour,
          colour,
          onTap: r.exists ? () => navigateToInstance(wref, r.pageId) : null,
        );
      },
    );
  }

  Widget _pillBody(BuildContext context, String text, Color border, Color fg, {VoidCallback? onTap}) {
    final pill = Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: kSurfaceContainerLow,
        borderRadius: BorderRadius.circular(4),
        border: Border.all(color: border.withValues(alpha: 0.5)),
      ),
      child: Text(text, style: Theme.of(context).textTheme.labelSmall?.copyWith(color: fg)),
    );
    if (onTap == null) return pill;
    return InkWell(
      borderRadius: BorderRadius.circular(4),
      onTap: onTap,
      child: pill,
    );
  }
}

/// Cached `resolve()` result per markup string so a single
/// `[[wikilink]]` doesn't fire the tool call once per render.
final _resolvedRefProvider = FutureProvider.family<ResolveResult, String>((ref, markup) {
  return ref.watch(escurelClientProvider).resolve(markup);
});
