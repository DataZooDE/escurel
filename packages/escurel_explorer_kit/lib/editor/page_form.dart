/// The page edit form — shared by the "edit existing instance" path
/// (mounted by [EntityEditor] in edit mode) and the "create new
/// instance" path (mounted in a dialog from the catalogue).
///
/// Drives the field set from the skill's required/optional frontmatter
/// (union with keys already on the page) and writes back a canonical
/// `---\n<k>: <v>\n---\n\n<body>` markdown document. Validates via the
/// client's `validate` tool before writing through `update_page`.
///
/// **Semantics contract** (for the rodney a11y smoke): the interactive
/// nodes carry stable `Semantics(label:)` tokens — `save-page`,
/// `cancel-edit`, `delete-page`, `field:<key>`, `body-editor`,
/// `validation-status`.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../config/env.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

/// Structural frontmatter keys the form keeps read-only — they define
/// the page's identity and must not be retyped by the operator.
const _structuralKeys = {'type', 'skill', 'id'};

/// Frontmatter fields with a closed value domain — rendered as a dropdown
/// rather than a free-text box so only valid values can be entered.
/// `visibility` is the legacy summary of the skill's `acl:` model and the
/// backend recognises exactly these (anything else degrades to `public`).
const _enumFields = <String, List<String>>{
  'visibility': ['public', 'owner'],
};

/// Serialise a draft back into a canonical markdown page.
///
/// Frontmatter is emitted as `key: value` lines in a stable order
/// (structural keys first), list values as YAML flow sequences
/// (`[a, b]`), and the body follows after the closing `---`.
String serializePage(Map<String, dynamic> frontmatter, String body) {
  final keys = <String>[
    for (final k in ['type', 'skill', 'id']) if (frontmatter.containsKey(k)) k,
    ...frontmatter.keys.where((k) => !_structuralKeys.contains(k)),
  ];
  final buf = StringBuffer('---\n');
  for (final k in keys) {
    buf.write('$k: ${_yamlValue(frontmatter[k])}\n');
  }
  buf.write('---\n\n');
  buf.write(body.trimRight());
  buf.write('\n');
  return buf.toString();
}

String _yamlValue(dynamic v) {
  if (v == null) return '';
  if (v is List) {
    return '[${v.map((e) => e.toString()).join(', ')}]';
  }
  final s = v.toString();
  // Quote values whose punctuation would otherwise confuse the YAML
  // parser (a leading `[`, an embedded `:` followed by space, …). The
  // wikilink form `[[skill::id]]` is left unquoted — the fixture/server
  // parser recovers it as a nested flow sequence on purpose.
  final needsQuote = s.contains(': ') ||
      (s.startsWith('[') && !s.startsWith('[[')) ||
      s.startsWith('{') ||
      s.startsWith('#') ||
      s.startsWith('"');
  if (needsQuote) return '"${s.replaceAll('"', r'\"')}"';
  return s;
}

/// The columns the form renders for [skill] over an existing/new
/// frontmatter map: the skill's required + optional fields, unioned
/// with any keys already present, minus the structural identity keys
/// (which render as a read-only header instead).
List<_FieldSpec> _fieldSpecsFor(SkillSummary skill, Map<String, dynamic> frontmatter) {
  final required = skill.requiredFrontmatter.toSet();
  final ordered = <String>[
    ...skill.requiredFrontmatter,
    ...skill.optionalFrontmatter.where((k) => !required.contains(k)),
    ...frontmatter.keys.where(
      (k) => !required.contains(k) && !skill.optionalFrontmatter.contains(k),
    ),
  ].where((k) => !_structuralKeys.contains(k)).toList();
  // De-dupe while preserving order.
  final seen = <String>{};
  return [
    for (final k in ordered)
      if (seen.add(k)) _FieldSpec(key: k, isRequired: required.contains(k)),
  ];
}

class _FieldSpec {
  const _FieldSpec({required this.key, required this.isRequired});
  final String key;
  final bool isRequired;
}

/// The edit form. [pageId] is the durable page id being written; for a
/// new instance it is computed by the caller. [isNew] gates the delete
/// action (only existing instances can be tombstoned) and the id-input
/// (a new instance lets the operator type its id).
class PageEditForm extends ConsumerStatefulWidget {
  const PageEditForm({
    super.key,
    required this.skill,
    required this.pageId,
    required this.baseVersion,
    required this.isNew,
    this.onDone,
  });

  final SkillSummary skill;
  final String pageId;
  final String? baseVersion;
  final bool isNew;

  /// Called after a successful save/delete with the page id that should
  /// be focused (the saved page, or null to clear focus on delete).
  final void Function(String? focusPageId)? onDone;

  @override
  ConsumerState<PageEditForm> createState() => _PageEditFormState();
}

class _PageEditFormState extends ConsumerState<PageEditForm> {
  final _controllers = <String, TextEditingController>{};
  late final TextEditingController _body;
  late final TextEditingController _newId;

  @override
  void initState() {
    super.initState();
    final draft = ref.read(pageDraftProvider) ??
        PageDraft(frontmatter: {}, body: '');
    for (final spec in _fieldSpecsFor(widget.skill, draft.frontmatter)) {
      final v = draft.frontmatter[spec.key];
      _controllers[spec.key] = TextEditingController(text: _displayValue(v));
    }
    _body = TextEditingController(text: draft.body);
    _newId = TextEditingController(text: (draft.frontmatter['id'] as String?) ?? '');
  }

  @override
  void dispose() {
    for (final c in _controllers.values) {
      c.dispose();
    }
    _body.dispose();
    _newId.dispose();
    super.dispose();
  }

  String _displayValue(dynamic v) {
    if (v == null) return '';
    if (v is List) return v.map((e) => e.toString()).join(', ');
    return v.toString();
  }

  /// Read the live controller state back into a frontmatter map (lists
  /// recovered from comma-separated inputs for keys the page declared as
  /// lists or whose value looks like a list).
  Map<String, dynamic> _collectFrontmatter() {
    final draft = ref.read(pageDraftProvider);
    final out = <String, dynamic>{...?draft?.frontmatter};
    // Keep structural keys as-is; refresh editable ones from controllers.
    for (final entry in _controllers.entries) {
      final raw = entry.value.text;
      final wasList = draft?.frontmatter[entry.key] is List;
      out[entry.key] = wasList ? _splitList(raw) : raw;
    }
    return out;
  }

  List<String> _splitList(String raw) => raw
      .split(',')
      .map((s) => s.trim())
      .where((s) => s.isNotEmpty)
      .toList();

  /// The durable page id the write targets. For an existing page that
  /// is its open handle; for a new instance it's the canonical markdown
  /// path the server keys writes by.
  String _effectivePageId() {
    if (!widget.isNew) return widget.pageId;
    final id = _slug(_newId.text);
    return 'markdown/instances/${widget.skill.id}/$id.md';
  }

  /// The id to focus after a create — whatever the active transport's
  /// `expand`/catalogue resolves. The real server (HTTP) keys instances by
  /// the canonical markdown page id; the in-memory fixture keys them by the
  /// `<skill>__<id>` handle. Focus the matching one so the new page opens
  /// (not the empty-state placeholder).
  String _focusId() {
    if (!widget.isNew) return widget.pageId;
    final id = _slug(_newId.text);
    if (ref.read(envProvider).mode == AppMode.http) {
      return 'markdown/instances/${widget.skill.id}/$id.md';
    }
    return '${widget.skill.id}__$id';
  }

  String _slug(String raw) => raw.trim().toLowerCase().replaceAll(RegExp(r'[^a-z0-9._-]+'), '-');

  String _buildContent({Map<String, dynamic>? overrideFrontmatter, String? overrideBody}) {
    final fm = overrideFrontmatter ?? _collectFrontmatter();
    // Ensure structural keys are present and coherent.
    fm['type'] = 'instance';
    fm['skill'] = widget.skill.id;
    fm['id'] = widget.isNew ? _slug(_newId.text) : (fm['id'] as String? ?? '');
    return serializePage(fm, overrideBody ?? _body.text);
  }

  Future<void> _validate() async {
    final client = ref.read(escurelClientProvider);
    try {
      final res = await client.validate(_buildContent(), asPageId: _effectivePageId());
      if (!mounted) return;
      ref.read(pageValidationProvider.notifier).state = res.issues;
    } catch (_) {
      // Validation transport errors are surfaced on Save; ignore here.
    }
  }

  Future<void> _save() async {
    if (widget.isNew && _slug(_newId.text).isEmpty) {
      ref.read(pageSaveProvider.notifier).state =
          const SaveState(status: SaveStatus.error, message: 'Bitte eine ID angeben.');
      return;
    }
    final client = ref.read(escurelClientProvider);
    final pageId = _effectivePageId();
    final content = _buildContent();
    ref.read(pageSaveProvider.notifier).state = const SaveState(status: SaveStatus.saving);
    try {
      final v = await client.validate(content, asPageId: pageId);
      ref.read(pageValidationProvider.notifier).state = v.issues;
      if (!v.isOk) {
        ref.read(pageSaveProvider.notifier).state = const SaveState(
          status: SaveStatus.error,
          message: 'Validierung fehlgeschlagen.',
        );
        return;
      }
      final res = await client.updatePage(
        pageId,
        content,
        baseVersion: widget.isNew ? null : widget.baseVersion,
      );
      if (!res.ok) {
        ref.read(pageValidationProvider.notifier).state = res.issues;
        ref.read(pageSaveProvider.notifier).state = SaveState(
          status: SaveStatus.error,
          message: res.issues.isEmpty
              ? 'Speichern abgelehnt.'
              : res.issues.map((i) => i.message).join('; '),
        );
        return;
      }
      _exitAndRefresh(focus: _focusId());
    } catch (e) {
      ref.read(pageSaveProvider.notifier).state =
          SaveState(status: SaveStatus.error, message: '$e');
    }
  }

  Future<void> _delete() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Instanz löschen?'),
        content: Text('„${widget.pageId}" wird als gelöscht markiert (Tombstone).'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('Abbrechen'),
          ),
          Semantics(
            label: PageFormKeys.confirmDelete,
            identifier: PageFormKeys.confirmDelete,
            button: true,
            onTap: () => Navigator.of(ctx).pop(true),
            excludeSemantics: true,
            child: FilledButton(
              onPressed: () => Navigator.of(ctx).pop(true),
              child: const Text('Löschen'),
            ),
          ),
        ],
      ),
    );
    if (confirmed != true) return;

    final client = ref.read(escurelClientProvider);
    final fm = _collectFrontmatter();
    fm['type'] = 'instance';
    fm['skill'] = widget.skill.id;
    fm['status'] = 'erased';
    final content = serializePage(fm, _body.text);
    ref.read(pageSaveProvider.notifier).state = const SaveState(status: SaveStatus.saving);
    try {
      final res = await client.updatePage(widget.pageId, content, baseVersion: widget.baseVersion);
      if (!res.ok) {
        ref.read(pageSaveProvider.notifier).state = SaveState(
          status: SaveStatus.error,
          message: res.issues.isEmpty ? 'Löschen abgelehnt.' : res.issues.map((i) => i.message).join('; '),
        );
        return;
      }
      _exitAndRefresh(focus: null);
    } catch (e) {
      ref.read(pageSaveProvider.notifier).state = SaveState(status: SaveStatus.error, message: '$e');
    }
  }

  void _cancel() {
    ref.read(editModeProvider.notifier).state = false;
    ref.read(pageDraftProvider.notifier).state = null;
    ref.read(pageValidationProvider.notifier).state = const [];
    ref.read(pageSaveProvider.notifier).state = SaveState.idle;
    widget.onDone?.call(null);
  }

  void _exitAndRefresh({required String? focus}) {
    ref.read(editModeProvider.notifier).state = false;
    ref.read(pageDraftProvider.notifier).state = null;
    ref.read(pageValidationProvider.notifier).state = const [];
    ref.read(pageSaveProvider.notifier).state = SaveState.idle;
    // Reload the focused page (and the catalogue, so a new/erased
    // instance appears/disappears).
    ref.invalidate(currentPageProvider);
    ref.invalidate(skillsCatalogueProvider);
    ref.invalidate(instancesProvider);
    widget.onDone?.call(focus);
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final issues = ref.watch(pageValidationProvider);
    final save = ref.watch(pageSaveProvider);
    final hasError = issues.any((i) => i.severity == IssueSeverity.error);
    final specs = _fieldSpecsFor(widget.skill, ref.read(pageDraftProvider)?.frontmatter ?? const {});

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (widget.isNew) ...[
          Text('ID', style: text.labelLarge?.copyWith(color: kOnSurfaceVariant)),
          const SizedBox(height: 4),
          Semantics(
            label: PageFormKeys.idField,
            textField: true,
            identifier: PageFormKeys.idField,
            child: TextField(
              controller: _newId,
              decoration: const InputDecoration(
                hintText: 'kurz-und-slug-ish',
                border: OutlineInputBorder(),
                isDense: true,
              ),
              onChanged: (_) => _validate(),
            ),
          ),
          const SizedBox(height: 16),
        ],
        Text('Frontmatter', style: text.titleSmall),
        const SizedBox(height: 8),
        for (final spec in specs) ...[
          _FieldRow(
            spec: spec,
            controller: _controllers[spec.key]!,
            onChanged: _validate,
          ),
          const SizedBox(height: 10),
        ],
        const SizedBox(height: 8),
        Text('Body', style: text.titleSmall),
        const SizedBox(height: 8),
        Semantics(
          label: PageFormKeys.bodyEditor,
          textField: true,
          identifier: PageFormKeys.bodyEditor,
          child: TextField(
            controller: _body,
            maxLines: 10,
            style: const TextStyle(fontFamily: 'monospace', fontFamilyFallback: ['Courier']),
            decoration: const InputDecoration(border: OutlineInputBorder()),
            onChanged: (_) => _validate(),
          ),
        ),
        const SizedBox(height: 16),
        _ValidationStatus(issues: issues, saveMessage: save.status == SaveStatus.error ? save.message : null),
        const SizedBox(height: 12),
        Row(
          children: [
            Semantics(
              label: PageFormKeys.save,
              identifier: PageFormKeys.save,
              button: true,
              onTap: (hasError || save.status == SaveStatus.saving) ? null : _save,
              excludeSemantics: true,
              child: FilledButton(
                onPressed: (hasError || save.status == SaveStatus.saving) ? null : _save,
                child: const Text('Speichern'),
              ),
            ),
            const SizedBox(width: 8),
            Semantics(
              label: PageFormKeys.cancel,
              identifier: PageFormKeys.cancel,
              button: true,
              onTap: _cancel,
              excludeSemantics: true,
              child: OutlinedButton(
                onPressed: _cancel,
                child: const Text('Abbrechen'),
              ),
            ),
            const Spacer(),
            if (!widget.isNew)
              Semantics(
                label: PageFormKeys.delete,
                identifier: PageFormKeys.delete,
                button: true,
                onTap: _delete,
                excludeSemantics: true,
                child: TextButton(
                  onPressed: _delete,
                  style: TextButton.styleFrom(foregroundColor: kError),
                  child: const Text('Löschen'),
                ),
              ),
          ],
        ),
      ],
    );
  }
}

class _FieldRow extends StatelessWidget {
  const _FieldRow({required this.spec, required this.controller, required this.onChanged});

  final _FieldSpec spec;
  final TextEditingController controller;
  final VoidCallback onChanged;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final options = _enumFields[spec.key];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(spec.key, style: text.labelLarge?.copyWith(color: kOnSurfaceVariant)),
            if (spec.isRequired)
              Text(' *', style: text.labelLarge?.copyWith(color: kError)),
          ],
        ),
        const SizedBox(height: 4),
        Semantics(
          label: '${PageFormKeys.fieldPrefix}${spec.key}',
          identifier: '${PageFormKeys.fieldPrefix}${spec.key}',
          textField: options == null,
          child: options == null
              ? TextField(
                  controller: controller,
                  decoration:
                      const InputDecoration(border: OutlineInputBorder(), isDense: true),
                  onChanged: (_) => onChanged(),
                )
              : _EnumDropdown(controller: controller, options: options, onChanged: onChanged),
        ),
      ],
    );
  }
}

/// A dropdown for a closed-domain field. Bound to the same
/// [TextEditingController] the text path uses, so save/validation read the
/// selection back unchanged. Defaults to the first option when the field is
/// empty, and preserves an unexpected stored value by offering it too.
class _EnumDropdown extends StatelessWidget {
  const _EnumDropdown({
    required this.controller,
    required this.options,
    required this.onChanged,
  });

  final TextEditingController controller;
  final List<String> options;
  final VoidCallback onChanged;

  @override
  Widget build(BuildContext context) {
    var current = controller.text.trim();
    if (current.isEmpty) {
      // Seed the controller so an untouched save persists the default.
      current = options.first;
      controller.text = current;
    }
    final items = [
      ...options,
      if (!options.contains(current)) current, // keep a non-standard value selectable
    ];
    return DropdownButtonFormField<String>(
      initialValue: current,
      decoration: const InputDecoration(border: OutlineInputBorder(), isDense: true),
      items: [
        for (final o in items) DropdownMenuItem<String>(value: o, child: Text(o)),
      ],
      onChanged: (v) {
        if (v == null) return;
        controller.text = v;
        onChanged();
      },
    );
  }
}

class _ValidationStatus extends StatelessWidget {
  const _ValidationStatus({required this.issues, this.saveMessage});

  final List<Issue> issues;
  final String? saveMessage;

  Color _color(BuildContext context, IssueSeverity sev) {
    final ext = Theme.of(context).extension<ExplorerColors>() ?? const ExplorerColors.light();
    return switch (sev) {
      IssueSeverity.error => kError,
      IssueSeverity.warning => ext.warning,
      IssueSeverity.info => ext.info,
    };
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    if (issues.isEmpty && saveMessage == null) {
      return Semantics(
        label: PageFormKeys.validationStatus,
        identifier: PageFormKeys.validationStatus,
        child: Text('Keine Probleme.', style: text.bodySmall?.copyWith(color: kOnSurfaceVariant)),
      );
    }
    return Semantics(
      label: PageFormKeys.validationStatus,
      identifier: PageFormKeys.validationStatus,
      container: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final i in issues)
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: Text(
                '${i.severity.name}: ${i.message}',
                style: text.bodySmall?.copyWith(color: _color(context, i.severity)),
              ),
            ),
          if (saveMessage != null)
            Text(saveMessage!, style: text.bodySmall?.copyWith(color: kError)),
        ],
      ),
    );
  }
}

/// Stable semantics tokens for the edit form (rodney a11y selectors).
class PageFormKeys {
  static const editPage = 'edit-page';
  static const save = 'save-page';
  static const cancel = 'cancel-edit';
  static const delete = 'delete-page';
  static const confirmDelete = 'confirm-delete';
  static const idField = 'instance-id';
  static const bodyEditor = 'body-editor';
  static const validationStatus = 'validation-status';
  static const fieldPrefix = 'field:';
}
