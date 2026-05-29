/// Source inbox — the left rail from the mockups. A reverse-chronological
/// feed of the artifacts agents ingested (email / meeting / doc), each
/// showing its source channel, a relative timestamp, and a provenance
/// badge (EXTRACTED · AUTO-PROMOTED). Tapping a row focuses the artifact
/// so the centre region reads it.
///
/// Pure composition over the real `list_instances(skill, order_by: at desc)`
/// tool (PR-5's frontmatter filter is what lets later views narrow this
/// by `source`); no fixture data lives here.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';

class InboxList extends ConsumerWidget {
  const InboxList({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final inbox = ref.watch(inboxArtifactsProvider);
    final focused = ref.watch(currentPageIdProvider);

    return Semantics(
      label: 'inbox',
      container: true,
      explicitChildNodes: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 14, 16, 8),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text('INBOX', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
                inbox.maybeWhen(
                  data: (a) => Text('${a.length}', style: text.labelSmall?.copyWith(color: kOutline)),
                  orElse: () => const SizedBox.shrink(),
                ),
              ],
            ),
          ),
          const Divider(height: 1),
          Expanded(
            child: inbox.when(
              loading: () => const Center(child: CircularProgressIndicator(strokeWidth: 2)),
              error: (e, _) => Padding(
                padding: const EdgeInsets.all(16),
                child: Text('inbox error: $e', style: text.bodySmall?.copyWith(color: kError)),
              ),
              data: (artifacts) {
                if (artifacts.isEmpty) {
                  return Padding(
                    padding: const EdgeInsets.all(16),
                    child: Text('No source artifacts', style: text.bodySmall?.copyWith(color: kOutline)),
                  );
                }
                return ListView.separated(
                  padding: EdgeInsets.zero,
                  itemCount: artifacts.length,
                  separatorBuilder: (_, _) => const Divider(height: 1),
                  itemBuilder: (context, i) => _ArtifactTile(
                    artifact: artifacts[i],
                    selected: artifacts[i].pageId == focused,
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

class _ArtifactTile extends ConsumerWidget {
  const _ArtifactTile({required this.artifact, required this.selected});
  final Artifact artifact;
  final bool selected;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'inbox-item',
      button: true,
      selected: selected,
      onTap: () => ref.read(currentPageIdProvider.notifier).state = artifact.pageId,
      excludeSemantics: true,
      child: InkWell(
        onTap: () => ref.read(currentPageIdProvider.notifier).state = artifact.pageId,
        child: Container(
          color: selected ? kSurfaceContainerHigh : null,
          padding: const EdgeInsets.fromLTRB(16, 10, 12, 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  _SourceChip(source: artifact.source, skill: artifact.skill),
                  const Spacer(),
                  Text(
                    _shortWhen(artifact.at),
                    style: text.labelSmall?.copyWith(color: kOutline),
                  ),
                ],
              ),
              const SizedBox(height: 6),
              Text(
                artifact.title,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: text.bodyMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w500),
              ),
              if (artifact.provenance.isNotEmpty) ...[
                const SizedBox(height: 6),
                _ProvenanceBadge(provenance: artifact.provenance),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// The ingest channel (gmail / meet / gcal / drive / agent) as a small
/// labelled chip with a leading glyph.
class _SourceChip extends StatelessWidget {
  const _SourceChip({required this.source, required this.skill});
  final String source;
  final String skill;

  @override
  Widget build(BuildContext context) {
    final (icon, label) = _sourceFace(source, skill);
    final text = Theme.of(context).textTheme;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 14, color: kPrimary),
        const SizedBox(width: 4),
        Text(label, style: text.labelSmall?.copyWith(color: kPrimary, fontWeight: FontWeight.w600)),
      ],
    );
  }
}

(IconData, String) _sourceFace(String source, String skill) {
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
      // Fall back to the artifact kind when the source is unknown.
      switch (skill) {
        case 'meeting':
          return (Icons.videocam_outlined, 'Meeting');
        case 'doc':
          return (Icons.description_outlined, 'Doc');
        default:
          return (Icons.inbox_outlined, source.isEmpty ? skill : source);
      }
  }
}

/// EXTRACTED (neutral/success) vs AUTO-PROMOTED (highlighted) provenance.
class _ProvenanceBadge extends StatelessWidget {
  const _ProvenanceBadge({required this.provenance});
  final String provenance;

  @override
  Widget build(BuildContext context) {
    final promoted = provenance.toUpperCase().contains('PROMOTED');
    final fg = promoted ? kOnSecondaryContainer : kSuccess;
    final bg = promoted ? kSecondaryContainer : kSuccess.withValues(alpha: 0.14);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(6)),
      child: Text(
        provenance.toUpperCase(),
        style: Theme.of(context)
            .textTheme
            .labelSmall
            ?.copyWith(color: fg, fontSize: 9, fontWeight: FontWeight.w700, letterSpacing: 0.5),
      ),
    );
  }
}

/// Compact `YYYY-MM-DD` (drops the time) from an RFC 3339 stamp; empty
/// stays empty.
String _shortWhen(String at) {
  if (at.isEmpty) return '';
  final t = at.indexOf('T');
  return t > 0 ? at.substring(0, t) : at;
}
