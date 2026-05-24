import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../md/wikilink.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/kind_chip.dart';
import '../widgets/wikilink_pill.dart';

/// Center pane — the entity editor. Renders the focused page's
/// frontmatter as a key/value table and the body as paragraph text
/// with inline `[[wikilink]]` pills.
class EntityEditor extends ConsumerWidget {
  const EntityEditor({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final async = ref.watch(currentPageProvider);
    final scheme = Theme.of(context).colorScheme;

    return Container(
      key: const ValueKey('pane.editor'),
      color: scheme.surface,
      child: async.when(
        loading: () => const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => Center(
          child: Text('$e', style: const TextStyle(color: kError)),
        ),
        data: (page) => page == null ? const _EmptyState() : _PageView(page: page),
      ),
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState();

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text('No page open', style: text.titleMedium?.copyWith(color: kOnSurfaceVariant)),
            const SizedBox(height: 4),
            Text(
              'Pick a skill or instance from the catalogue.',
              style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            ),
          ],
        ),
      ),
    );
  }
}

class _PageView extends StatelessWidget {
  const _PageView({required this.page});

  final ExpandResult page;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final title = (page.frontmatter['name'] as String?) ??
        (page.frontmatter['title'] as String?) ??
        page.pageId;

    return SingleChildScrollView(
      key: const ValueKey('entity_editor.scroll'),
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              KindChip(pageType: page.pageType),
              const SizedBox(width: 8),
              Text(page.pageId, style: text.bodySmall?.copyWith(color: kOnSurfaceVariant)),
            ],
          ),
          const SizedBox(height: 8),
          Text(title, style: text.displayMedium, key: const ValueKey('entity_editor.title')),
          const SizedBox(height: 16),
          _FrontmatterTable(fields: page.frontmatter),
          const SizedBox(height: 24),
          _BodyBlock(body: page.body),
        ],
      ),
    );
  }
}

class _FrontmatterTable extends StatelessWidget {
  const _FrontmatterTable({required this.fields});

  final Map<String, dynamic> fields;

  @override
  Widget build(BuildContext context) {
    final entries = fields.entries.toList();
    final text = Theme.of(context).textTheme;
    return Container(
      key: const ValueKey('entity_editor.frontmatter'),
      decoration: BoxDecoration(
        color: kSurfaceContainerLow,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: kOutlineVariant),
      ),
      child: Column(
        children: [
          for (var i = 0; i < entries.length; i++) ...[
            if (i > 0) const Divider(height: 1, color: kOutlineVariant),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 140,
                    child: Text(
                      entries[i].key,
                      style: text.labelLarge?.copyWith(color: kOnSurfaceVariant),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(child: _ValueRender(value: entries[i].value)),
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _ValueRender extends StatelessWidget {
  const _ValueRender({required this.value});

  final dynamic value;

  @override
  Widget build(BuildContext context) {
    if (value == null) {
      return Text('—', style: Theme.of(context).textTheme.bodySmall?.copyWith(color: kOnSurfaceVariant));
    }
    final raw = value.toString();
    final refs = parseWikilinks(raw);
    if (refs.isEmpty) {
      return Text(raw, style: Theme.of(context).textTheme.bodyMedium);
    }
    return Wrap(
      spacing: 6,
      runSpacing: 4,
      children: refs.map((r) => WikilinkPill(ref: r)).toList(),
    );
  }
}

class _BodyBlock extends StatelessWidget {
  const _BodyBlock({required this.body});

  final String body;

  @override
  Widget build(BuildContext context) {
    final lines = body.trimLeft().split('\n');
    final text = Theme.of(context).textTheme;
    return Column(
      key: const ValueKey('entity_editor.body'),
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (final line in lines) _renderLine(context, line, text),
      ],
    );
  }

  Widget _renderLine(BuildContext context, String line, TextTheme text) {
    if (line.startsWith('# ')) {
      return Padding(
        padding: const EdgeInsets.only(top: 12, bottom: 6),
        child: Text(line.substring(2), style: text.headlineMedium),
      );
    }
    if (line.startsWith('## ')) {
      return Padding(
        padding: const EdgeInsets.only(top: 12, bottom: 4),
        child: Text(line.substring(3), style: text.titleLarge),
      );
    }
    if (line.isEmpty) return const SizedBox(height: 8);
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: _ParagraphWithPills(text: line),
    );
  }
}

/// Inline rendering of a paragraph with `[[wikilink]]` substituted
/// for [WikilinkPill] widgets via [WidgetSpan]. Not a full markdown
/// renderer — heading + paragraph + pill is enough for the editor
/// merge gate; richer renderer arrives with PR-5.
class _ParagraphWithPills extends StatelessWidget {
  const _ParagraphWithPills({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final pattern = RegExp(r'\[\[([^\[\]\r\n]+?)\]\]');
    final matches = pattern.allMatches(text).toList();
    if (matches.isEmpty) {
      return Text(text, style: Theme.of(context).textTheme.bodyMedium);
    }

    final spans = <InlineSpan>[];
    var cursor = 0;
    for (final m in matches) {
      if (m.start > cursor) {
        spans.add(TextSpan(text: text.substring(cursor, m.start)));
      }
      final refs = parseWikilinks(m[0]!);
      if (refs.isEmpty) {
        spans.add(TextSpan(text: m[0]));
      } else {
        spans.add(WidgetSpan(
          alignment: PlaceholderAlignment.middle,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 2),
            child: WikilinkPill(ref: refs.first),
          ),
        ));
      }
      cursor = m.end;
    }
    if (cursor < text.length) {
      spans.add(TextSpan(text: text.substring(cursor)));
    }
    return RichText(
      text: TextSpan(
        style: Theme.of(context).textTheme.bodyMedium?.copyWith(color: kOnSurface),
        children: spans,
      ),
    );
  }
}
