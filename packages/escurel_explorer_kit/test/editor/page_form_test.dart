// Unit tests for the editing serialization + fixture write path. No
// mocks: a real FixtureEscurelClient validates/updates an in-memory
// corpus, and `serializePage` round-trips through the real frontmatter
// parser.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/page_form.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart' as md;
import 'package:flutter_test/flutter_test.dart';

const _noteSkill = '---\n'
    'type: skill\n'
    'id: note\n'
    'required_frontmatter: [title]\n'
    'optional_frontmatter: [tags]\n'
    '---\n\n# note\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
      writeEnabled: true,
      skillFiles: {'note.md': _noteSkill},
      instanceFiles: {
        'note__a.md': '---\ntype: instance\nskill: note\nid: a\ntitle: A\n---\n\n# A\n',
      },
    );

void main() {
  test('serializePage emits structural keys first and parses back', () {
    final md.Page page = md.parse(serializePage(
      {'skill': 'note', 'type': 'instance', 'id': 'x', 'title': 'Hello', 'tags': ['a', 'b']},
      '# Body\n\ntext',
    ));
    expect(page.frontmatter.fields['type'], 'instance');
    expect(page.frontmatter.fields['skill'], 'note');
    expect(page.frontmatter.fields['id'], 'x');
    expect(page.frontmatter.fields['title'], 'Hello');
    expect(page.frontmatter.fields['tags'], ['a', 'b']);
    expect(page.body, contains('# Body'));
  });

  test('serializePage quotes values that would break the YAML parser', () {
    final page = md.parse(serializePage(
      {'type': 'instance', 'skill': 'note', 'id': 'x', 'title': 'a: b'},
      '# x',
    ));
    expect(page.frontmatter.fields['title'], 'a: b');
  });

  test('fixture validate: missing required structural key is an error', () async {
    final res = await _client().validate('---\ntype: instance\nskill: note\n---\n\n# x\n');
    expect(res.isOk, isFalse);
    expect(res.issues.any((i) => i.severity == IssueSeverity.error), isTrue);
  });

  test('fixture validate: skill-declared required frontmatter is enforced', () async {
    // `title` is required by the note skill; omit it.
    final res = await _client().validate('---\ntype: instance\nskill: note\nid: q\n---\n\n# q\n');
    expect(res.isOk, isFalse);
    expect(res.issues.map((i) => i.message).join(), contains('title'));
  });

  test('fixture updatePage upserts the page and bumps the version', () async {
    final client = _client();
    final res = await client.updatePage(
      'markdown/instances/note/b.md',
      '---\ntype: instance\nskill: note\nid: b\ntitle: B\n---\n\n# B\n',
    );
    expect(res.ok, isTrue);
    expect(res.newVersion, isNotNull);
    // Keyed <skill>__<id>, visible to expand + listInstances.
    final page = await client.expand('note__b');
    expect(page.frontmatter['title'], 'B');
    final ids = (await client.listInstances('note')).map((i) => i.id).toSet();
    expect(ids, containsAll({'note__a', 'note__b'}));
  });

  test('fixture updatePage rejects a stale baseVersion', () async {
    final client = _client();
    // First write to establish a head version.
    final first = await client.updatePage(
      'markdown/instances/note/a.md',
      '---\ntype: instance\nskill: note\nid: a\ntitle: A2\n---\n\n# A2\n',
    );
    expect(first.ok, isTrue);
    // A write against a now-stale base version is rejected.
    final stale = await client.updatePage(
      'note__a',
      '---\ntype: instance\nskill: note\nid: a\ntitle: A3\n---\n\n# A3\n',
      baseVersion: 'fx-999',
    );
    expect(stale.ok, isFalse);
    expect(stale.issues.first.code, 'stale_base_version');
  });

  test('read-only fixture still throws on the write path', () async {
    final ro = FixtureEscurelClient.fromSources(
      skillFiles: {'note.md': _noteSkill},
      instanceFiles: const {},
    );
    expect(() => ro.validate('x'), throwsA(anything));
    final v = await ro.version();
    expect(v.capabilities, isNot(contains(BackendCapability.agentWriteTools)));
  });

  test('listSkills parses visibility + owner_field with defaults', () async {
    final client = FixtureEscurelClient.fromSources(
      skillFiles: {
        'note.md': _noteSkill,
        'private_profile.md': '---\ntype: skill\nid: private_profile\n'
            'visibility: owner\nowner_field: owner\n---\n\n# p\n',
      },
      instanceFiles: const {},
    );
    final skills = {for (final s in await client.listSkills()) s.id: s};
    expect(skills['note']!.visibility, 'public');
    expect(skills['note']!.ownerField, isNull);
    expect(skills['private_profile']!.visibility, 'owner');
    expect(skills['private_profile']!.ownerField, 'owner');
  });
}
