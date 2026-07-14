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
  skillFiles: const {
    'customer.md': _customerSkill,
    'contact.md': _contactSkill,
  },
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
      final contact = (await client.listSkills()).firstWhere(
        (s) => s.id == 'contact',
      );
      expect(contact.requiredFrontmatter, ['name', 'customer']);
    });

    test('listInstances returns instances of one skill', () async {
      final contacts = await client.listInstances('contact');
      expect(contacts.map((i) => i.id).toList(), ['contact__dora']);
    });

    test('listInstances filters by frontmatter equality', () async {
      final de = await client.listInstances(
        'customer',
        filter: const {'country': 'DE'},
      );
      expect(de, hasLength(1));
      final us = await client.listInstances(
        'customer',
        filter: const {'country': 'US'},
      );
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
      final out = await client.neighbours(
        'customer__acme',
        direction: LinkDirection.outgoing,
      );
      // `dst` is the link's slug (matching the gateway wire shape).
      expect(out.map((n) => n.dst), contains('dora'));
    });

    test(
      'neighbours: incoming edges into customer include the contact',
      () async {
        final inc = await client.neighbours(
          'customer__acme',
          direction: LinkDirection.incoming,
        );
        expect(inc.map((n) => n.src), contains('contact__dora'));
      },
    );

    test(
      'search hits via case-insensitive substring across body + id + skill',
      () async {
        final r = await client.search(q: 'dora');
        expect(r.hits.map((h) => h.pageId), contains('contact__dora'));
      },
    );

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

  // ── layers + packs (REQ-LAYER-03/04, REQ-SUB-01 fixture parity) ──

  group('FixtureEscurelClient (layers + packs)', () {
    const basePlaybook = '''---
type: skill
id: playbook
description: Firm-authored engagement playbook (crm-essentials v1).
layer: base@crm-essentials@v1
required_frontmatter: [name]
stage_gates: 4
---

# playbook

Firm-authored canonical playbook.
''';

    const overlayPlaybook = '''---
type: skill
id: playbook
description: Demo-specialised engagement playbook.
required_frontmatter: [name]
stage_gates: 5
---

# playbook

Demo-specialised playbook.
''';

    const baseOnlyEscalation = '''---
type: skill
id: escalation
description: Firm-authored escalation ladder.
layer: base@crm-essentials@v1
---

# escalation

Firm-authored escalation ladder.
''';

    FixtureEscurelClient layered() => FixtureEscurelClient.fromSources(
      skillFiles: const {
        'customer.md': _customerSkill,
        'base/crm-essentials/skills/playbook.md': basePlaybook,
        'playbook.md': overlayPlaybook,
        'base/crm-essentials/skills/escalation.md': baseOnlyEscalation,
      },
      instanceFiles: const {},
    );

    test('a base-layer skill page reports its layer pin', () async {
      final skills = await layered().listSkills();
      final escalation = skills.firstWhere((s) => s.id == 'escalation');
      expect(escalation.layer, 'base@crm-essentials@v1');
      expect(escalation.isBaseLayer, isTrue);
      expect(escalation.shadows, isNull);
    });

    test('a plain skill reports the overlay default', () async {
      final skills = await layered().listSkills();
      final customer = skills.firstWhere((s) => s.id == 'customer');
      expect(customer.layer, 'overlay');
      expect(customer.isBaseLayer, isFalse);
      expect(customer.shadows, isNull);
    });

    test('an overlay shadowing a base skill folds to ONE catalogue entry '
        'carrying the shadowed pin', () async {
      final skills = await layered().listSkills();
      final playbooks = skills.where((s) => s.id == 'playbook').toList();
      expect(playbooks, hasLength(1));
      final playbook = playbooks.single;
      // The overlay wins; it carries the shadowed base's pin.
      expect(playbook.layer, 'overlay');
      expect(playbook.shadows, 'base@crm-essentials@v1');
      expect(playbook.description, 'Demo-specialised engagement playbook.');
    });

    test(
      'listPacks synthesizes one subscription per distinct base pack',
      () async {
        final packs = await layered().listPacks();
        expect(packs, hasLength(1));
        expect(packs.single.packId, 'crm-essentials');
        expect(packs.single.version, 1);
        expect(packs.single.vertical, 'demo');
        expect(packs.single.publisher, 'demo');
      },
    );

    test('a node with no base pages still reports zero packs', () async {
      expect(await _inlineClient().listPacks(), isEmpty);
    });

    test('expand of the shadowing overlay carries the shadow object', () async {
      final client = layered();
      final page = await client.expand('playbook');
      expect(page.shadow, isNotNull);
      expect(page.shadow!.pack, 'base@crm-essentials@v1');
      expect(page.shadow!.basePageId, 'base/crm-essentials/skills/playbook.md');
      // The base frontmatter is exposed so drift stays visible.
      expect(page.shadow!.base['stage_gates'], 4);
      expect(page.frontmatter['stage_gates'], 5);

      // INV-SHADOW: the base page stays pristine and expandable.
      final base = await client.expand(page.shadow!.basePageId);
      expect(base.frontmatter['stage_gates'], 4);
      expect(base.shadow, isNull);
    });

    test('resolve prefers the overlay over its shadowed base '
        '(and still finds a base-only skill)', () async {
      final client = layered();
      // Shadowed id → the overlay page wins (server precedence:
      // base pages order last).
      final playbook = await client.resolve('[[playbook]]');
      expect(playbook.exists, isTrue);
      expect(playbook.pageId, 'playbook');
      // A base-only skill still resolves (to its base page) so the
      // catalogue's skill-page tap works in fixture mode.
      final escalation = await client.resolve('[[escalation]]');
      expect(escalation.exists, isTrue);
      expect(escalation.pageId, 'base/crm-essentials/skills/escalation.md');
    });

    test(
      'resolve handles the reserved skill:: namespace (definition pages)',
      () async {
        final client = layered();
        // `[[skill::<id>]]` targets the skill DEFINITION page itself
        // (issue #212) — the server matches page_type = skill, never a
        // literal `skill` column (read.rs).
        final customer = await client.resolve('[[skill::customer]]');
        expect(customer.exists, isTrue);
        expect(customer.pageId, 'customer');
        expect(customer.pageType, md.PageType.skill);
        // A shadowed id resolves to the OVERLAY definition page.
        final playbook = await client.resolve('[[skill::playbook]]');
        expect(playbook.exists, isTrue);
        expect(playbook.pageId, 'playbook');
        expect(playbook.pageType, md.PageType.skill);
      },
    );

    test('expand of a non-shadowing page carries no shadow', () async {
      final client = layered();
      expect((await client.expand('customer')).shadow, isNull);
      // A base-only skill (nothing shadows it) also carries none.
      expect(
        (await client.expand(
          'base/crm-essentials/skills/escalation.md',
        )).shadow,
        isNull,
      );
    });

    test(
      'unsubscribePack drops the pack pages, the pack row, and the shadow',
      () async {
        final client = layered();
        final r = await client.unsubscribePack('crm-essentials');
        expect(r.pack, 'crm-essentials');
        expect(r.pagesRemoved, 2); // playbook base + escalation base
        expect(await client.listPacks(), isEmpty);
        // The overlay survives untouched; it simply stops shadowing.
        final playbook = (await client.listSkills()).firstWhere(
          (s) => s.id == 'playbook',
        );
        expect(playbook.shadows, isNull);
        expect((await client.expand('playbook')).shadow, isNull);
        // The base-only skill is gone from the catalogue.
        expect(
          (await client.listSkills()).where((s) => s.id == 'escalation'),
          isEmpty,
        );
      },
    );

    test('unsubscribePack of an unknown pack refuses', () async {
      await expectLater(
        layered().unsubscribePack('not-subscribed'),
        throwsA(
          isA<EscurelToolException>().having(
            (e) => e.code,
            'code',
            'pack_not_subscribed',
          ),
        ),
      );
    });

    test(
      'importPack / rebasePack are not implemented in fixture mode',
      () async {
        await expectLater(
          layered().importPack('{}', 'AAAA'),
          throwsA(isA<EscurelUnsupportedException>()),
        );
        await expectLater(
          layered().rebasePack('{}', 'AAAA'),
          throwsA(isA<EscurelUnsupportedException>()),
        );
      },
    );
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
        containsAll([
          'escurel',
          'customer',
          'contact',
          'engagement',
          'lead',
          'opportunity',
          'project',
        ]),
      );
    });

    test('the Hoffmann chain traverses end-to-end', () async {
      final inc = await client.neighbours(
        'contact__hoffmann',
        direction: LinkDirection.incoming,
      );
      final srcs = inc.map((n) => n.src).toSet();
      expect(
        srcs,
        containsAll([
          'engagement__hoffmann-intro',
          'lead__hoffmann-followup',
          'opportunity__hoffmann-pilot',
        ]),
      );
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
