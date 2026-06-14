@TestOn('vm')
library;

import 'dart:io';

import 'package:escurel_explorer_kit/client/errors.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart' as md;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

// ── inline fixture (always runs — no dependency on examples/) ───
//
// The inline corpus mirrors a tiny slice of examples/crm-demo/ so
// the FixtureEscurelClient is exercised even when run from a
// stacked PR branch that does not yet contain the examples tree.

const _customerSkill = '''---
type: skill
id: customer
description: A buying organisation.
required_frontmatter: [name, country]
---

# customer

body
''';

const _contactSkill = '''---
type: skill
id: contact
description: An individual person at a customer.
required_frontmatter: [name, customer]
---

# contact

body
''';

const _customerInst = '''---
type: instance
skill: customer
id: acme
name: Acme Ltd
country: DE
---

# Acme Ltd

Primary champion: [[contact::dora]].
''';

const _contactInst = '''---
type: instance
skill: contact
id: dora
name: Dora Doe
customer: [[customer::acme]]
role: VP Engineering
---

# Dora Doe

''';

FixtureEscurelClient _inlineClient() => FixtureEscurelClient.fromSources(
      skillFiles: const {'customer.md': _customerSkill, 'contact.md': _contactSkill},
      instanceFiles: const {
        'customer__acme.md': _customerInst,
        'contact__dora.md': _contactInst,
      },
    );

void main() {
  group('FixtureEscurelClient (inline corpus)', () {
    late EscurelClient client;

    setUpAll(() => client = _inlineClient());
    tearDownAll(() => client.close());

    test('listSkills returns the two seeded skills', () async {
      final ids = (await client.listSkills()).map((s) => s.id).toSet();
      expect(ids, {'customer', 'contact'});
    });

    test('listSkills carries required_frontmatter from yaml', () async {
      final contact = (await client.listSkills()).firstWhere((s) => s.id == 'contact');
      expect(contact.requiredFrontmatter, ['name', 'customer']);
    });

    test('listInstances returns instances of one skill', () async {
      final contacts = await client.listInstances('contact');
      expect(contacts.map((i) => i.id).toList(), ['contact__dora']);
    });

    test('listInstances filters by frontmatter equality', () async {
      final de = await client.listInstances('customer', filter: const {'country': 'DE'});
      expect(de, hasLength(1));
      final us = await client.listInstances('customer', filter: const {'country': 'US'});
      expect(us, isEmpty);
    });

    test('resolve finds existing instance via typed wikilink', () async {
      final r = await client.resolve('[[contact::dora]]');
      expect(r.exists, isTrue);
      expect(r.pageId, 'contact__dora');
      expect(r.pageType, md.PageType.instance);
    });

    test('resolve reports exists:false for an unknown id', () async {
      final r = await client.resolve('[[contact::ghost]]');
      expect(r.exists, isFalse);
    });

    test('expand returns frontmatter + body + outgoing wikilinks', () async {
      final page = await client.expand('customer__acme');
      expect(page.frontmatter['country'], 'DE');
      expect(page.wikilinksOut, contains('[[contact::dora]]'));
    });

    test('expand throws EscurelToolException for unknown page id', () async {
      await expectLater(
        client.expand('customer__not-real'),
        throwsA(isA<EscurelToolException>()),
      );
    });

    test('neighbours: outgoing edges from customer reach contact', () async {
      final out = await client.neighbours('customer__acme', direction: LinkDirection.outgoing);
      // `dst` is the link's slug (matching the gateway wire shape).
      expect(out.map((n) => n.dst), contains('dora'));
    });

    test('neighbours: incoming edges into customer include the contact', () async {
      final inc = await client.neighbours('customer__acme', direction: LinkDirection.incoming);
      expect(inc.map((n) => n.src), contains('contact__dora'));
    });

    test('search hits via case-insensitive substring across body + id + skill', () async {
      final r = await client.search(q: 'dora');
      expect(r.hits.map((h) => h.pageId), contains('contact__dora'));
    });

    test('write tools surface as EscurelUnsupportedException', () async {
      await expectLater(
        client.updatePage('x', 'body'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
    });

    test('version declares only agentReadTools', () async {
      final v = await client.version();
      expect(v.capabilities, {BackendCapability.agentReadTools});
    });
  });

  // ── directory pass (runs only when examples/ is present) ──────
  //
  // After the examples/crm-demo branch (#11) merges to main, this
  // runs the same shape of assertions against the real seed.

  group('FixtureEscurelClient (examples/crm-demo on disk)', () {
    final examplesDir = _findExamplesDir();
    if (examplesDir == null) {
      test('skipped — examples/crm-demo not present on this branch', () {});
      return;
    }

    late EscurelClient client;
    setUpAll(() {
      client = FixtureEscurelClient.fromSources(
        skillFiles: _loadDir(p.join(examplesDir, 'skills')),
        instanceFiles: _loadDir(p.join(examplesDir, 'instances')),
      );
    });
    tearDownAll(() => client.close());

    test('seven seeded skills present', () async {
      final ids = (await client.listSkills()).map((s) => s.id).toSet();
      expect(
        ids,
        containsAll(['escurel', 'customer', 'contact', 'engagement', 'lead', 'opportunity', 'project']),
      );
    });

    test('the Hoffmann chain traverses end-to-end', () async {
      final inc = await client.neighbours('contact__hoffmann', direction: LinkDirection.incoming);
      final srcs = inc.map((n) => n.src).toSet();
      expect(srcs, containsAll([
        'engagement__hoffmann-intro',
        'lead__hoffmann-followup',
        'opportunity__hoffmann-pilot',
      ]));
    });
  });
}

String? _findExamplesDir() {
  var dir = Directory.current;
  for (var i = 0; i < 6; i++) {
    final candidate = Directory(p.join(dir.path, 'examples', 'crm-demo'));
    if (candidate.existsSync()) return candidate.path;
    final parent = dir.parent;
    if (parent.path == dir.path) break;
    dir = parent;
  }
  return null;
}

Map<String, String> _loadDir(String path) {
  final out = <String, String>{};
  for (final entry in Directory(path).listSync()) {
    if (entry is File && entry.path.endsWith('.md')) {
      out[p.basename(entry.path)] = entry.readAsStringSync();
    }
  }
  return out;
}
