/// LEFT pane (M7) — the **event view** of the focused memory: a
/// master-detail of the instance's event history, the open event's
/// preview, and the general inbox below. Opening an event sets
/// [openEventProvider] only — it does NOT change the pinned entity.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';

class EventPane extends ConsumerWidget {
  const EventPane({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final events = ref.watch(entityEventsProvider);
    final inbox = ref.watch(inboxEventsProvider);
    final open = ref.watch(openEventProvider);

    return Semantics(
      label: 'event-pane',
      container: true,
      explicitChildNodes: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Event-type (SOURCES) filter — one chip per processing skill.
          const _SourcesFilter(),
          // Master: the focused instance's event history.
          _SectionHeader(
            label: 'EVENTS',
            trailing: events.maybeWhen(data: (e) => '${e.length}', orElse: () => null),
          ),
          Expanded(
            flex: 5,
            child: Semantics(
              label: 'event-history',
              container: true,
              explicitChildNodes: true,
              child: events.when(
                loading: () => const _Loading(),
                error: (e, _) => _Error('$e'),
                data: (list) => list.isEmpty
                    ? const _Empty('No events for this instance')
                    : ListView.separated(
                        padding: EdgeInsets.zero,
                        itemCount: list.length,
                        separatorBuilder: (_, _) => const Divider(height: 1),
                        itemBuilder: (_, i) => _EventTile(
                          event: list[i],
                          selected: list[i].eventId == open,
                          onTap: () => ref.read(openEventProvider.notifier).state = list[i].eventId,
                        ),
                      ),
              ),
            ),
          ),
          // Detail: the open event's preview.
          const Divider(height: 1),
          const Expanded(flex: 4, child: _EventDetail()),
          // Inbox below.
          const Divider(height: 1),
          _SectionHeader(
            label: 'INBOX',
            trailing: inbox.maybeWhen(data: (e) => '${e.length}', orElse: () => null),
          ),
          Expanded(
            flex: 3,
            child: Semantics(
              label: 'inbox',
              container: true,
              explicitChildNodes: true,
              child: inbox.when(
                loading: () => const _Loading(),
                error: (e, _) => _Error('$e'),
                data: (list) => list.isEmpty
                    ? const _Empty('Inbox empty')
                    : ListView.separated(
                        padding: EdgeInsets.zero,
                        itemCount: list.length,
                        separatorBuilder: (_, _) => const Divider(height: 1),
                        itemBuilder: (_, i) => _EventTile(
                          event: list[i],
                          selected: list[i].eventId == open,
                          inbox: true,
                          onTap: () => ref.read(openEventProvider.notifier).state = list[i].eventId,
                        ),
                      ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The event-type filter row: one chip per distinct `label_skill` in the
/// focused instance's history. Tapping a chip filters the event list to
/// that processing skill; tapping the active chip clears the filter.
class _SourcesFilter extends ConsumerWidget {
  const _SourcesFilter();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final sources = ref.watch(availableSourcesProvider);
    final selected = ref.watch(eventSourceFilterProvider);
    if (sources.isEmpty) return const SizedBox.shrink();
    return Semantics(
      label: 'sources-filter',
      container: true,
      explicitChildNodes: true,
      child: Container(
        color: kSurfaceContainerLow,
        padding: const EdgeInsets.fromLTRB(10, 8, 10, 8),
        child: Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final s in sources)
              _SourceChip(
                source: s,
                active: selected == s,
                onTap: () => ref.read(eventSourceFilterProvider.notifier).state = selected == s ? null : s,
              ),
          ],
        ),
      ),
    );
  }
}

class _SourceChip extends StatelessWidget {
  const _SourceChip({required this.source, required this.active, required this.onTap});
  final String source;
  final bool active;
  final VoidCallback onTap;
  @override
  Widget build(BuildContext context) {
    final (icon, label) = sourceFace(source, source);
    return Semantics(
      label: 'source-chip:$source',
      button: true,
      selected: active,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(999),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
          decoration: BoxDecoration(
            color: active ? kPrimary : kSurfaceContainer,
            borderRadius: BorderRadius.circular(999),
            border: Border.all(color: active ? kPrimary : kOutlineVariant),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(icon, size: 13, color: active ? kSurface : kOnSurfaceVariant),
              const SizedBox(width: 4),
              Text(
                label,
                style: Theme.of(context)
                    .textTheme
                    .labelSmall
                    ?.copyWith(color: active ? kSurface : kOnSurfaceVariant, fontWeight: FontWeight.w600),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// (icon, label) for an event's source/mime — the source label is a
/// human face on the event's processing skill.
(IconData, String) sourceFace(String source, String labelSkill) {
  switch (source) {
    case 'gmail':
      return (Icons.mail_outline, 'Gmail');
    case 'meet':
      return (Icons.videocam_outlined, 'Meet');
    case 'gcal':
      return (Icons.event_outlined, 'Calendar');
    case 'drive':
      return (Icons.description_outlined, 'Docs');
    case 'agent':
      return (Icons.smart_toy_outlined, 'Agent');
    default:
      return (Icons.bolt_outlined, source.isEmpty ? labelSkill : source);
  }
}

String shortWhen(String? at) {
  if (at == null || at.isEmpty) return '';
  final t = at.indexOf('T');
  return t > 0 ? at.substring(0, t) : at;
}

class _EventTile extends StatelessWidget {
  const _EventTile({
    required this.event,
    required this.selected,
    required this.onTap,
    this.inbox = false,
  });
  final Event event;
  final bool selected;
  final bool inbox;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final (icon, label) = sourceFace(event.source, event.labelSkill);
    final prov = (event.provenance['provenance'] as String?) ?? '';
    return Semantics(
      label: inbox ? 'inbox-item' : 'event-item',
      button: true,
      selected: selected,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        child: Container(
          color: selected ? kSurfaceContainerHigh : null,
          padding: const EdgeInsets.fromLTRB(14, 9, 12, 9),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(icon, size: 14, color: kPrimary),
                  const SizedBox(width: 4),
                  Text(label, style: text.labelSmall?.copyWith(color: kPrimary, fontWeight: FontWeight.w600)),
                  const Spacer(),
                  Text(shortWhen(event.at), style: text.labelSmall?.copyWith(color: kOutline)),
                ],
              ),
              const SizedBox(height: 5),
              Text(
                event.title.isEmpty ? event.eventId : event.title,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: text.bodyMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w500),
              ),
              if (prov.isNotEmpty) ...[
                const SizedBox(height: 5),
                _Badge(prov),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _EventDetail extends ConsumerWidget {
  const _EventDetail();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final e = ref.watch(openEventDetailProvider);
    return Semantics(
      label: 'event-detail',
      container: true,
      explicitChildNodes: true,
      child: e == null
          ? const _Empty('Select an event to preview')
          : SingleChildScrollView(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      _Chip(sourceFace(e.source, e.labelSkill).$2),
                      const SizedBox(width: 6),
                      Text(e.mime, style: text.labelSmall?.copyWith(color: kOutline)),
                      const Spacer(),
                      Text(shortWhen(e.at), style: text.labelSmall?.copyWith(color: kOutline)),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Text(e.title, style: text.titleMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w700)),
                  const SizedBox(height: 8),
                  Text(e.body, style: text.bodyMedium?.copyWith(color: kOnSurfaceVariant, height: 1.4)),
                ],
              ),
            ),
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({required this.label, this.trailing});
  final String label;
  final String? trailing;
  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Container(
      color: kSurfaceContainerLow,
      padding: const EdgeInsets.fromLTRB(14, 8, 12, 8),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.spaceBetween,
        children: [
          Text(label, style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
          if (trailing != null) Text(trailing!, style: text.labelSmall?.copyWith(color: kOutline)),
        ],
      ),
    );
  }
}

class _Badge extends StatelessWidget {
  const _Badge(this.text);
  final String text;
  @override
  Widget build(BuildContext context) {
    final promoted = text.toUpperCase().contains('PROMOTED');
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: promoted ? kSecondaryContainer : kSuccess.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        text.toUpperCase(),
        style: Theme.of(context)
            .textTheme
            .labelSmall
            ?.copyWith(color: promoted ? kOnSecondaryContainer : kSuccess, fontSize: 9, fontWeight: FontWeight.w700),
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(color: kSecondaryContainer, borderRadius: BorderRadius.circular(6)),
        child: Text(text,
            style: Theme.of(context).textTheme.labelSmall?.copyWith(color: kOnSecondaryContainer, fontSize: 9)),
      );
}

class _Loading extends StatelessWidget {
  const _Loading();
  @override
  Widget build(BuildContext context) => const Center(child: CircularProgressIndicator(strokeWidth: 2));
}

class _Error extends StatelessWidget {
  const _Error(this.msg);
  final String msg;
  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.all(16),
        child: Text('error: $msg', style: Theme.of(context).textTheme.bodySmall?.copyWith(color: kError)),
      );
}

class _Empty extends StatelessWidget {
  const _Empty(this.msg);
  final String msg;
  @override
  Widget build(BuildContext context) => Center(
        child: Text(msg, style: Theme.of(context).textTheme.bodySmall?.copyWith(color: kOutline)),
      );
}
