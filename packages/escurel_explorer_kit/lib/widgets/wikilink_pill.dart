import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/escurel_client.dart';
import '../client/models.dart';
import '../md/wikilink.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'instance_skill_link.dart';

/// Clickable pill rendering a `[[skill::id]]` reference.
///
/// Resolves via [EscurelClient.resolve] on first build; the pill's
/// outline turns red when the link is dangling and grey otherwise.
/// A resolvable, *typed* pill carries the instance↔skill dual (see
/// [InstanceSkillLink]): default tap → the linked instance; shift-click
/// or the hover chip → the reference's skill. Untyped links just open
/// their target.
class WikilinkPill extends ConsumerWidget {
  const WikilinkPill({super.key, required this.ref});

  final WikilinkRef ref;

  @override
  Widget build(BuildContext context, WidgetRef wref) {
    final markup = ref.toMarkup();
    final resolved = wref.watch(_resolvedRefProvider(markup));
    final text = ref.alias ?? (ref.skill != null ? '${ref.skill}::${ref.id}' : ref.id ?? '?');

    return resolved.when(
      loading: () => _pillBody(context, text, kOutlineVariant, kOnSurfaceVariant),
      error: (e, _) => _pillBody(context, text, kError, kError),
      data: (r) {
        final colour = r.exists ? kPrimary : kError;
        final pill = _pillBody(context, text, colour, colour);
        if (!r.exists) return pill;
        final skill = ref.skill;
        // Typed links (`[[skill::id]]`) carry the dual; bare links just
        // navigate to their target.
        if (skill == null || skill.isEmpty) {
          return InkWell(
            borderRadius: BorderRadius.circular(4),
            onTap: () => navigateToInstance(wref, r.pageId),
            child: pill,
          );
        }
        return InstanceSkillLink(
          borderRadius: BorderRadius.circular(4),
          skillLabel: skill,
          onPrimary: () => navigateToInstance(wref, r.pageId),
          onSkill: () => focusSkill(wref, skill),
          child: pill,
        );
      },
    );
  }

  Widget _pillBody(BuildContext context, String text, Color border, Color fg) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: kSurfaceContainerLow,
        borderRadius: BorderRadius.circular(4),
        border: Border.all(color: border.withValues(alpha: 0.5)),
      ),
      child: Text(text, style: Theme.of(context).textTheme.labelSmall?.copyWith(color: fg)),
    );
  }
}

/// Cached `resolve()` result per markup string so a single
/// `[[wikilink]]` doesn't fire the tool call once per render.
final _resolvedRefProvider = FutureProvider.family<ResolveResult, String>((ref, markup) {
  return ref.watch(escurelClientProvider).resolve(markup);
});
