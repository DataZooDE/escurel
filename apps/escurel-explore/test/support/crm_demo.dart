@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/fixture_escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:flutter_test/flutter_test.dart';

/// Locate the repo's `examples/crm-demo` by walking up from the test CWD
/// (the flutter package root). Lets widget tests run against the **real**
/// seed corpus — the single source of truth shared with the live gateway.
Directory crmDemoDir() {
  var dir = Directory.current;
  for (var i = 0; i < 8; i++) {
    final candidate = Directory('${dir.path}/examples/crm-demo');
    if (candidate.existsSync()) return candidate;
    final parent = dir.parent;
    if (parent.path == dir.path) break;
    dir = parent;
  }
  throw StateError('could not locate examples/crm-demo from ${Directory.current.path}');
}

/// `markdown/instances/engagement__hoffmann-spine.md` →
/// `engagement__hoffmann-spine` (the fixture client's short page id).
String _short(String instancePath) => instancePath.split('/').last.replaceAll('.md', '');

/// A real [FixtureEscurelClient] over the actual `examples/crm-demo`
/// corpus — skills, base instances, `events.json`, `history.json`. No
/// mocks: the same data the live gateway seeds, parsed in-process.
EscurelClient crmDemoClient() {
  final dir = crmDemoDir();

  Map<String, String> readDir(String sub, bool Function(String) keep) {
    final out = <String, String>{};
    for (final f in Directory('${dir.path}/$sub').listSync().whereType<File>()) {
      final name = f.uri.pathSegments.last;
      if (!name.endsWith('.md') || !keep(name)) continue;
      out[name] = f.readAsStringSync();
    }
    return out;
  }

  final skillFiles = readDir('skills', (_) => true);
  // Base instances only — skip the `.a`/`.b` scenario overlays.
  final instanceFiles =
      readDir('instances', (n) => !n.endsWith('.a.md') && !n.endsWith('.b.md'));

  final events = <Event>[];
  final eventsFile = File('${dir.path}/events.json');
  if (eventsFile.existsSync()) {
    final raw = jsonDecode(eventsFile.readAsStringSync()) as List;
    for (var i = 0; i < raw.length; i++) {
      final e = raw[i] as Map<String, dynamic>;
      events.add(Event(
        eventId: 'crm-ev-$i',
        at: e['at'] as String?,
        source: (e['source'] as String?) ?? '',
        mime: (e['mime'] as String?) ?? '',
        labelSkill: (e['label_skill'] as String?) ?? '',
        instancePageId: e['instance'] != null ? _short(e['instance'] as String) : null,
        status: (e['status'] as String?) ?? 'inbox',
        title: (e['title'] as String?) ?? '',
        body: (e['body'] as String?) ?? '',
        provenance: (e['provenance'] as Map?)?.cast<String, dynamic>() ?? const {},
      ));
    }
  }

  final snapshots = <String, List<String>>{};
  final historyFile = File('${dir.path}/history.json');
  if (historyFile.existsSync()) {
    final raw = jsonDecode(historyFile.readAsStringSync()) as List;
    for (final p in raw.cast<Map<String, dynamic>>()) {
      snapshots[_short(p['page_id'] as String)] = (p['states'] as List)
          .cast<Map<String, dynamic>>()
          .map((s) => s['taken_at'] as String)
          .toList();
    }
  }

  return FixtureEscurelClient.fromSources(
    skillFiles: skillFiles,
    instanceFiles: instanceFiles,
    events: events,
    snapshots: snapshots,
  );
}

/// The engagement spine's short page id — the instance most widget tests
/// focus (richly connected: many events, backlinks, a snapshot timeline).
const crmDemoSpineId = 'engagement__hoffmann-spine';
