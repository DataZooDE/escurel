@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:dio/dio.dart';
import 'package:escurel_explorer_kit/client/errors.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/http_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart' as md;
import 'package:flutter_test/flutter_test.dart';

/// Stands up a real `dart:io` HttpServer on localhost:0 and lets
/// each test inject canned responses keyed by the tool name. True
/// end-to-end: the dio client speaks to a real socket. Proves the
/// MCP/HTTP wire envelope (JSON-RPC 2.0, `method = "tools/call"`,
/// `params = { name, arguments }`) is correctly produced and
/// consumed.
class _MockMcpServer {
  _MockMcpServer._(this._server, this.baseUrl);

  final HttpServer _server;
  final String baseUrl;

  /// Map of tool name → handler that gets the JSON-RPC `arguments`
  /// and returns the `result` map.
  final Map<
    String,
    FutureOr<Map<String, dynamic>> Function(Map<String, dynamic>)
  >
  toolHandlers = {};

  /// Map of route path → handler for non-MCP endpoints (/healthz, /version).
  final Map<String, FutureOr<Map<String, dynamic>?> Function()> routeHandlers =
      {};

  /// Map of route path → plain-text body (text/plain). escurel's `/version`
  /// answers with a bare version string, not JSON — this serves that shape.
  final Map<String, String> rawRouteHandlers = {};

  /// Pre-built tool error to return on the next call (overrides handler).
  ({int code, String message})? nextError;

  static Future<_MockMcpServer> start() async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final base = 'http://${server.address.host}:${server.port}';
    final mock = _MockMcpServer._(server, base);
    mock._listen();
    return mock;
  }

  void _listen() {
    _server.listen((req) async {
      if (req.method == 'POST' && req.uri.path == '/mcp') {
        final body =
            jsonDecode(await utf8.decoder.bind(req).join())
                as Map<String, dynamic>;
        if (nextError != null) {
          await _respondJson(req, 200, {
            'jsonrpc': '2.0',
            'id': body['id'],
            'error': {'code': nextError!.code, 'message': nextError!.message},
          });
          nextError = null;
          return;
        }
        final params = body['params'] as Map<String, dynamic>;
        final name = params['name'] as String;
        final args = (params['arguments'] as Map<String, dynamic>?) ?? const {};
        final handler = toolHandlers[name];
        if (handler == null) {
          await _respondJson(req, 200, {
            'jsonrpc': '2.0',
            'id': body['id'],
            'error': {'code': 404, 'message': 'no handler for $name'},
          });
          return;
        }
        final result = await handler(args);
        await _respondJson(req, 200, {
          'jsonrpc': '2.0',
          'id': body['id'],
          'result': result,
        });
        return;
      }
      final raw = rawRouteHandlers[req.uri.path];
      if (raw != null) {
        req.response.statusCode = 200;
        req.response.headers.contentType = ContentType.text;
        req.response.write(raw);
        await req.response.close();
        return;
      }
      final handler = routeHandlers[req.uri.path];
      if (handler != null) {
        final result = await handler();
        if (result == null) {
          await _respondJson(req, 200, {});
        } else {
          await _respondJson(req, 200, result);
        }
        return;
      }
      req.response.statusCode = 404;
      await req.response.close();
    });
  }

  Future<void> _respondJson(HttpRequest req, int status, Object body) async {
    req.response.statusCode = status;
    req.response.headers.contentType = ContentType.json;
    req.response.write(jsonEncode(body));
    await req.response.close();
  }

  Future<void> stop() => _server.close(force: true);
}

EscurelClient _client(_MockMcpServer mock) =>
    HttpEscurelClient(baseUrl: mock.baseUrl, dio: Dio());

void main() {
  late _MockMcpServer mock;
  late EscurelClient client;

  setUp(() async {
    mock = await _MockMcpServer.start();
    client = _client(mock);
  });

  tearDown(() async {
    client.close();
    await mock.stop();
  });

  group('MCP envelope', () {
    test(
      'search posts JSON-RPC 2.0 to /mcp with method=tools/call and correct args',
      () async {
        Map<String, dynamic>? receivedArgs;
        mock.toolHandlers['search'] = (args) {
          receivedArgs = args;
          return {
            'hits': [
              {
                'page_id': 'contact__hoffmann',
                'skill': 'contact',
                'score': 0.92,
              },
              {
                'page_id': 'engagement__hoffmann-intro',
                'skill': 'engagement',
                'score': 0.71,
              },
            ],
          };
        };

        final r = await client.search(q: 'hoffmann', k: 5, skill: 'contact');

        expect(receivedArgs, {
          'q': 'hoffmann',
          'k': 5,
          'granularity': 'block',
          'page_type': 'any',
          'skill': 'contact',
        });
        expect(r.hits.map((h) => h.pageId).toList(), [
          'contact__hoffmann',
          'engagement__hoffmann-intro',
        ]);
        expect(r.hits.first.score, closeTo(0.92, 1e-6));
      },
    );

    test('resolve unmarshals page_type into the md.PageType enum', () async {
      mock.toolHandlers['resolve'] = (args) => {
        'page_id': args['wikilink'],
        'skill': 'opportunity',
        'page_type': 'instance',
        'exists': true,
        'description': 'pilot opportunity',
      };

      final r = await client.resolve('[[opportunity::hoffmann-pilot]]');
      expect(r.exists, isTrue);
      expect(r.pageType, md.PageType.instance);
      expect(r.description, 'pilot opportunity');
    });

    test(
      'expand returns blocks + outgoing wikilinks shaped per spec',
      () async {
        mock.toolHandlers['expand'] = (args) => {
          'page_id': args['page_id'],
          'skill': 'opportunity',
          'page_type': 'instance',
          'frontmatter': {'value_eur': 60000, 'status': 'negotiating'},
          'body': '# Pilot\n\nMünchner Pharma',
          'blocks': [
            {'anchor': 'background', 'content': 'Hoffmann is champion.'},
          ],
          'wikilinks_out': [
            '[[customer::muenchner-pharma]]',
            '[[contact::hoffmann]]',
          ],
          'version': 'v3',
        };

        final r = await client.expand('opportunity__hoffmann-pilot');
        expect(r.frontmatter['value_eur'], 60000);
        expect(r.body, contains('Pilot'));
        expect(r.blocks, hasLength(1));
        expect(r.blocks.single.anchor, 'background');
        expect(r.wikilinksOut, contains('[[customer::muenchner-pharma]]'));
        expect(r.version, 'v3');
      },
    );

    test('neighbours returns Neighbour list shaped per spec', () async {
      mock.toolHandlers['neighbours'] = (args) => {
        'edges': [
          {
            'src': 'engagement__hoffmann-intro',
            'dst': args['page_id'],
            'link_skill': 'with',
          },
          {
            'src': 'lead__hoffmann-followup',
            'dst': args['page_id'],
            'link_skill': 'contact',
          },
        ],
      };
      final r = await client.neighbours(
        'contact__hoffmann',
        direction: LinkDirection.incoming,
      );
      expect(r, hasLength(2));
      expect(r.first.src, 'engagement__hoffmann-intro');
      expect(r.first.linkSkill, 'with');
    });

    test(
      'neighbours maps LinkDirection to the gateway wire values in|out|both',
      () async {
        // The gateway's neighbours tool rejects `incoming`/`outgoing`; the
        // client must send `in`/`out`/`both`. A real socket captures it.
        String? sent;
        mock.toolHandlers['neighbours'] = (args) {
          sent = args['direction'] as String?;
          return {'edges': const []};
        };
        await client.neighbours('p', direction: LinkDirection.incoming);
        expect(sent, 'in');
        await client.neighbours('p', direction: LinkDirection.outgoing);
        expect(sent, 'out');
        await client.neighbours('p', direction: LinkDirection.both);
        expect(sent, 'both');
      },
    );

    test('list_skills unmarshals required_frontmatter', () async {
      mock.toolHandlers['list_skills'] = (_) => {
        'skills': [
          {
            'id': 'engagement',
            'description': 'first-touch interaction',
            'required_frontmatter': ['at', 'with', 'channel'],
            'optional_frontmatter': ['outcome'],
          },
        ],
      };
      final r = await client.listSkills();
      expect(r, hasLength(1));
      expect(r.single.id, 'engagement');
      expect(r.single.requiredFrontmatter, ['at', 'with', 'channel']);
    });

    test(
      'list_skills parses the per-CRUD acl block + operatorEditable',
      () async {
        mock.toolHandlers['list_skills'] = (_) => {
          'skills': [
            {
              'id': 'incident',
              'description': 'a filed incident',
              'owner_field': 'reporter',
              'acl': {
                'read': ['public'],
                'create': ['owner'],
                'update': ['owner', 'moderator'],
                'delete': ['admin'],
              },
            },
            {
              'id': 'announcement',
              'description': 'admin-writable notice',
              'acl': {
                'read': ['public'],
                'update': ['admin'],
              },
            },
          ],
        };
        final r = await client.listSkills();
        final incident = r.firstWhere((s) => s.id == 'incident');
        expect(incident.acl?.read, ['public']);
        expect(incident.acl?.update, ['owner', 'moderator']);
        expect(incident.acl?.delete, ['admin']);
        // owner-scoped update ⇒ not operator-editable through the explorer.
        expect(incident.operatorEditable, isFalse);

        final announcement = r.firstWhere((s) => s.id == 'announcement');
        // update grants `admin` (no `owner`) ⇒ operator-editable.
        expect(announcement.operatorEditable, isTrue);
        expect(announcement.acl?.create, isNull); // omitted verb stays null
      },
    );

    test(
      'list_skills parses the layer pin; base layer gates operator editing',
      () async {
        mock.toolHandlers['list_skills'] = (_) => {
          'skills': [
            {
              'id': 'pallet-consolidation',
              'description': 'firm-authored, from the logistics pack',
              'layer': 'base@logistics-midmarket@v7',
              'acl': {
                'read': ['public'],
                'update': ['admin'],
              },
            },
            {
              'id': 'local-notes',
              'description': 'tenant-authored',
              // no `layer` key — older servers / plain pages.
            },
          ],
        };
        final r = await client.listSkills();
        final base = r.firstWhere((s) => s.id == 'pallet-consolidation');
        expect(base.layer, 'base@logistics-midmarket@v7');
        expect(base.isBaseLayer, isTrue);
        // operatorEditable stays layer-blind: it governs the skill's
        // INSTANCES, and overlay instances of a base skill are exactly
        // how a tenant specialises it. Only the base PAGE is read-only
        // (gated per-page; see state/layer_read_only_test.dart).
        expect(base.operatorEditable, isTrue);

        final plain = r.firstWhere((s) => s.id == 'local-notes');
        expect(plain.layer, 'overlay'); // absent ⇒ the overlay default
        expect(plain.isBaseLayer, isFalse);
        expect(plain.operatorEditable, isTrue);
      },
    );

    test(
      'expand surfaces the shadow object on a shadowing overlay skill page',
      () async {
        // REQ-LAYER-03: expanding a tenant overlay skill that shadows a
        // pack-imported base skill carries an additive `shadow` object —
        // the shadowed base's page id, pack pin, and frontmatter — so
        // drift stays visible, never silently masked.
        mock.toolHandlers['expand'] = (_) => {
          'page': {
            'page_id': 'markdown/skills/pallet-consolidation.md',
            'skill': 'pallet-consolidation',
            'page_type': 'skill',
          },
          'frontmatter': {
            'id': 'pallet-consolidation',
            'description': 'Acme-specialised procedure.',
          },
          'body': '# pallet-consolidation',
          'blocks': <Map<String, dynamic>>[],
          'wikilinks_out': <String>[],
          'shadow': {
            'base_page_id':
                'markdown/base/logistics-midmarket/skills/pallet-consolidation.md',
            'pack': 'base@logistics-midmarket@v7',
            'base': {
              'id': 'pallet-consolidation',
              'description': 'Firm-authored canonical procedure (v1).',
              'severity_threshold': 10,
            },
          },
        };
        final page = await client.expand(
          'markdown/skills/pallet-consolidation.md',
        );
        expect(page.shadow, isNotNull);
        expect(
          page.shadow!.basePageId,
          'markdown/base/logistics-midmarket/skills/pallet-consolidation.md',
        );
        expect(page.shadow!.pack, 'base@logistics-midmarket@v7');
        expect(page.shadow!.base['severity_threshold'], 10);

        // A page with no `shadow` in the reply parses to null (older
        // servers / non-shadowing pages).
        mock.toolHandlers['expand'] = (_) => {
          'page': {
            'page_id': 'markdown/skills/local-notes.md',
            'skill': 'local-notes',
            'page_type': 'skill',
          },
          'frontmatter': {'id': 'local-notes'},
          'body': '',
          'blocks': <Map<String, dynamic>>[],
          'wikilinks_out': <String>[],
        };
        final plain = await client.expand('markdown/skills/local-notes.md');
        expect(plain.shadow, isNull);
      },
    );

    test('run_stored_query parses the server schema into columns', () async {
      // The server emits `schema: [{name, type}]` (mcp.rs
      // tool_run_stored_query) — the client used to read a
      // non-existent `columns` key and silently dropped all column
      // metadata.
      mock.toolHandlers['run_stored_query'] = (_) => {
        'rows': [
          {'customer': 'acme', 'total': 12000},
        ],
        'schema': [
          {'name': 'customer', 'type': 'VARCHAR'},
          {'name': 'total', 'type': 'BIGINT'},
        ],
      };
      final r = await client.runStoredQuery('sales_by_customer');
      expect(r.rows, hasLength(1));
      expect(r.columns, hasLength(2), reason: 'schema must not be dropped');
      expect(r.columns.first.name, 'customer');
      expect(r.columns.first.dartType, 'VARCHAR');
    });

    test('list_packs parses the subscription pins', () async {
      mock.toolHandlers['list_packs'] = (_) => {
        'packs': [
          {
            'pack_id': 'logistics-midmarket',
            'version': 7,
            'vertical': 'logistics-midmarket',
            'publisher': 'hub.stuttgart-ai',
            'content_hash': 'sha256:abc',
          },
        ],
      };
      final r = await client.listPacks();
      expect(r, hasLength(1));
      expect(r.single.packId, 'logistics-midmarket');
      expect(r.single.version, 7);
      expect(r.single.vertical, 'logistics-midmarket');
      expect(r.single.publisher, 'hub.stuttgart-ai');
    });

    test(
      'import_pack sends the decoded manifest + tarball + mismatch flag',
      () async {
        Map<String, dynamic>? sent;
        mock.toolHandlers['import_pack'] = (args) {
          sent = args;
          return {
            'pack': 'logistics-midmarket',
            'version': 7,
            'vertical': 'logistics-midmarket',
            'pages_imported': 3,
            'layer': 'base@logistics-midmarket@v7',
          };
        };
        final r = await client.importPack(
          '{"id":"logistics-midmarket","version":7}',
          'dGFyYmFsbA==',
          allowVerticalMismatch: true,
        );
        // The manifest travels as a JSON OBJECT (the server deserializes
        // a PackManifest), not as the raw pasted string.
        expect(sent!['manifest'], {'id': 'logistics-midmarket', 'version': 7});
        expect(sent!['tarball_b64'], 'dGFyYmFsbA==');
        expect(sent!['allow_vertical_mismatch'], isTrue);
        expect(sent!['tenant_id'], '');
        expect(r.ok, isTrue);
        expect(r.pack, 'logistics-midmarket');
        expect(r.version, 7);
        expect(r.pagesImported, 3);
      },
    );

    test('importPack refuses a non-JSON manifest client-side', () async {
      await expectLater(
        client.importPack('not json', 'AAAA'),
        throwsA(
          isA<EscurelToolException>().having(
            (e) => e.code,
            'code',
            'manifest_invalid_json',
          ),
        ),
      );
    });

    test(
      'rebase_pack passes acknowledge_conflicts + the additive dry_run flag',
      () async {
        Map<String, dynamic>? sent;
        mock.toolHandlers['rebase_pack'] = (args) {
          sent = args;
          return {
            'ok': false,
            'issues': [
              {
                'severity': 'error',
                'code': 'rebase_conflict',
                'message': 'severity_threshold changed upstream',
              },
            ],
          };
        };
        final r = await client.rebasePack(
          '{"id":"logistics-midmarket","version":8}',
          'dGFyYmFsbA==',
          dryRun: true,
        );
        expect(sent!['acknowledge_conflicts'], isFalse);
        // dry_run is sent additively — the backend may not know the flag
        // yet, but an unknown field must not be silently dropped here.
        expect(sent!['dry_run'], isTrue);
        expect(r.ok, isFalse);
        expect(r.issues.single.code, 'rebase_conflict');
      },
    );

    test('unsubscribe_pack sends the pack id and parses the result', () async {
      Map<String, dynamic>? sent;
      mock.toolHandlers['unsubscribe_pack'] = (args) {
        sent = args;
        return {'pack': 'logistics-midmarket', 'pages_removed': 4};
      };
      final r = await client.unsubscribePack('logistics-midmarket');
      expect(sent!['pack_id'], 'logistics-midmarket');
      expect(r.pack, 'logistics-midmarket');
      expect(r.pagesRemoved, 4);
    });

    test('a pack tool error surfaces the server code verbatim', () async {
      mock.nextError = (
        code: -32000,
        message: 'pack_signature_invalid: manifest signature does not verify',
      );
      await expectLater(
        client.importPack('{"id":"p","version":1}', 'AAAA'),
        throwsA(
          isA<EscurelToolException>().having(
            (e) => e.message,
            'message',
            contains('pack_signature_invalid'),
          ),
        ),
      );
    });

    test(
      'list_instances maps filter to frontmatter_key/value + order_by + limit',
      () async {
        Map<String, dynamic>? receivedArgs;
        mock.toolHandlers['list_instances'] = (args) {
          receivedArgs = args;
          return {'instances': []};
        };
        await client.listInstances(
          'lead',
          filter: const {'status': 'qualified'},
          orderBy: 'opened desc',
          limit: 10,
        );
        // The single-entry filter map becomes the server's
        // (frontmatter_key, frontmatter_value) pair (PR-5).
        expect(receivedArgs, {
          'skill_id': 'lead',
          'frontmatter_key': 'status',
          'frontmatter_value': 'qualified',
          'order_by': 'opened desc',
          'limit': 10,
        });
      },
    );
  });

  group('errors', () {
    test('tool error envelope surfaces as EscurelToolException', () async {
      mock.nextError = (code: -32000, message: 'page not found');
      await expectLater(
        client.expand('not-a-real-page'),
        throwsA(
          isA<EscurelToolException>().having(
            (e) => e.message,
            'message',
            contains('page not found'),
          ),
        ),
      );
    });

    test('connection failure surfaces as EscurelTransportException', () async {
      final bad = HttpEscurelClient(baseUrl: 'http://127.0.0.1:1');
      await expectLater(
        bad.listSkills(),
        throwsA(isA<EscurelTransportException>()),
      );
      bad.close();
    });
  });

  group('substrate endpoints', () {
    test('healthz returns ok=true for HTTP 200', () async {
      mock.routeHandlers['/healthz'] = () => null;
      final h = await client.healthz();
      expect(h.ok, isTrue);
    });

    test(
      'version maps server capability strings into BackendCapability set',
      () async {
        mock.routeHandlers['/version'] = () => {
          'app': 'escurel-server',
          'version': '0.3.0',
          'git_sha': 'abc1234',
          'capabilities': [
            'agentReadTools',
            'agentWriteTools',
            'unknownCapName',
          ],
        };
        final v = await client.version();
        expect(v.version, '0.3.0');
        expect(v.capabilities, contains(BackendCapability.agentReadTools));
        expect(v.capabilities, contains(BackendCapability.agentWriteTools));
        // Unknown capability strings are dropped silently — forward compatibility.
        expect(
          v.capabilities.map((c) => c.name),
          isNot(contains('unknownCapName')),
        );
      },
    );

    test(
      'version tolerates a plain-text body (escurel returns a bare string)',
      () async {
        // escurel-server answers /version with `text/plain` "0.0.0-dev", not
        // JSON. The client must not throw a cast error; it surfaces the string.
        mock.rawRouteHandlers['/version'] = '0.0.0-dev';
        final v = await client.version();
        expect(v.version, '0.0.0-dev');
        expect(v.app, 'escurel-server');
        expect(v.capabilities, contains(BackendCapability.none));
      },
    );
  });

  group('not-yet-implemented surfaces', () {
    // Write, chat, session, and the implemented admin ops tools now
    // round-trip over /mcp (covered by the no-mock integration tests
    // in escurel-server). The LaneStore inspection tools have no
    // server-side MCP implementation yet, so they remain the only
    // surfaces that throw EscurelUnsupportedException.
    test('lane inspection tools throw EscurelUnsupportedException', () async {
      await expectLater(
        client.adminListLanes(),
        throwsA(isA<EscurelUnsupportedException>()),
      );
      await expectLater(
        client.adminLaneKeys('fs'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
      await expectLater(
        client.adminLaneBlob('fs', 'k'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
    });
  });

  group('external instance backends', () {
    test('list_skills surfaces backend kind + capabilities', () async {
      mock.toolHandlers['list_skills'] = (_) => {
        'skills': [
          {
            'id': 'customers',
            'description': 'EU customers',
            'required_frontmatter': <String>[],
            'optional_frontmatter': <String>[],
            'backend': {'kind': 'sql_view'},
            'capabilities': {
              'writable': false,
              'granularity': 'page',
              'search': 'late_materialized',
              'supports_crdt': false,
            },
          },
        ],
      };
      final skills = await client.listSkills();
      expect(skills.single.backendKind, 'sql_view');
      expect(skills.single.capabilities.writable, isFalse);
      expect(skills.single.capabilities.granularity, 'page');
    });

    test('expand surfaces a sql_view bounded projection', () async {
      mock.toolHandlers['expand'] = (_) => {
        'page': {
          'page_id': 'customers__eu',
          'skill': 'customers',
          'page_type': 'instance',
        },
        'frontmatter': {
          'backend_ref': {'kind': 'sql_view', 'view': 'vw_customers__eu'},
        },
        'body': '# EU',
        'blocks': <Map<String, dynamic>>[],
        'wikilinks_out': <String>[],
        'backend_projection': {
          'view': 'vw_customers__eu',
          'rows': [
            {'name': 'Acme'},
          ],
          'source': {'name': 'Acme'},
          'truncated': false,
        },
      };
      final page = await client.expand('customers__eu');
      expect(page.backendKind, 'sql_view');
      expect(page.backendProjection, isNotNull);
      expect(page.backendProjection!.rows.single['name'], 'Acme');
      expect(page.backendProjection!.degraded, isFalse);
    });

    test('expand surfaces a binding_degraded projection issue', () async {
      mock.toolHandlers['expand'] = (_) => {
        'page': {
          'page_id': 'customers__eu',
          'skill': 'customers',
          'page_type': 'instance',
        },
        'frontmatter': {
          'backend_ref': {'kind': 'sql_view', 'view': 'vw_customers__eu'},
        },
        'body': '',
        'blocks': <Map<String, dynamic>>[],
        'wikilinks_out': <String>[],
        'backend_projection': {
          'view': 'vw_customers__eu',
          'rows': <Map<String, dynamic>>[],
          'source': <String, dynamic>{},
          'issue': {'code': 'binding_degraded', 'message': 'drift'},
        },
      };
      final page = await client.expand('customers__eu');
      expect(page.backendProjection!.degraded, isTrue);
      expect(page.backendProjection!.issueCode, 'binding_degraded');
    });

    test('expand parses a remote (openapi/mcp) live projection', () async {
      // The remote wire shape (remote_backend::fetch_projection) differs
      // from sql_view: `source` is the endpoint NAME (a string, not the
      // projected-column map) and the values arrive under `fields`.
      mock.toolHandlers['expand'] = (_) => {
        'page': {
          'page_id': 'quote__aapl',
          'skill': 'quote',
          'page_type': 'instance',
        },
        'frontmatter': {
          'backend_ref': {'kind': 'openapi', 'endpoint': 'yahoo_finance'},
        },
        'body': '# AAPL',
        'blocks': <Map<String, dynamic>>[],
        'wikilinks_out': <String>[],
        'backend_projection': {
          'source': 'yahoo_finance',
          'fields': {'symbol': 'AAPL', 'price': 189.31, 'currency': 'USD'},
        },
      };
      final page = await client.expand('quote__aapl');
      expect(page.backendKind, 'openapi');
      final p = page.backendProjection!;
      expect(p.endpoint, 'yahoo_finance');
      expect(p.fields['symbol'], 'AAPL');
      expect(p.fields['price'], 189.31);
      expect(p.degraded, isFalse);
      // The sql_view-shaped members stay at their honest defaults.
      expect(p.rows, isEmpty);
      expect(p.source, isEmpty);
    });

    test('expand parses a degraded remote projection (string issue)', () async {
      // Exact live payload observed from a real openapi expand: the issue
      // is a plain STRING (unlike sql_view's {code, message} object) and
      // view/rows/source are entirely absent.
      mock.toolHandlers['expand'] = (_) => {
        'page': {
          'page_id': 'quote__aapl',
          'skill': 'quote',
          'page_type': 'instance',
        },
        'frontmatter': {
          'backend_ref': {'kind': 'openapi', 'endpoint': 'yahoo_finance'},
        },
        'body': '',
        'blocks': <Map<String, dynamic>>[],
        'wikilinks_out': <String>[],
        'backend_projection': {'issue': 'upstream status 429: null'},
      };
      final page = await client.expand('quote__aapl');
      final p = page.backendProjection!;
      expect(p.degraded, isTrue);
      expect(p.issueCode, isNull);
      expect(p.issueMessage, 'upstream status 429: null');
      expect(p.rows, isEmpty);
      expect(p.fields, isEmpty);
    });

    test('list_credentials parses without a secret field', () async {
      mock.toolHandlers['list_credentials'] = (_) => {
        'credentials': [
          {'name': 'crm_pg', 'connector': 'postgres', 'created_by': 'admin'},
        ],
      };
      final creds = await client.listCredentials();
      expect(creds.single.name, 'crm_pg');
      expect(creds.single.connector, 'postgres');
    });

    test('validate_bindings parses the health report', () async {
      mock.toolHandlers['validate_bindings'] = (_) => {
        'ok': false,
        'degraded': 1,
        'bindings': [
          {
            'page_id': 'p',
            'view': 'v',
            'status': 'binding_degraded',
            'detail': 'drift',
          },
        ],
      };
      final report = await client.validateBindings();
      expect(report.single.healthy, isFalse);
      expect(report.single.status, 'binding_degraded');
    });

    test('create_sql_instance returns the new page id', () async {
      Map<String, dynamic>? args;
      mock.toolHandlers['create_sql_instance'] = (a) {
        args = a;
        return {
          'page_id': 'markdown/instances/customers/us.md',
          'view': 'vw_customers__us',
        };
      };
      final pageId = await client.createSqlInstance(
        skill: 'customers',
        id: 'us',
      );
      expect(pageId, 'markdown/instances/customers/us.md');
      expect(args!['skill'], 'customers');
    });

    test('query_instance sends ref/params and parses rows+schema', () async {
      Map<String, dynamic>? args;
      mock.toolHandlers['query_instance'] = (a) {
        args = a;
        return {
          'rows': [
            {'name': 'Acme', 'total': 50},
          ],
          'schema': [
            {'name': 'name', 'type': 'VARCHAR'},
            {'name': 'total', 'type': 'BIGINT'},
          ],
          'truncated': true,
        };
      };
      final r = await client.queryInstance(
        '[[query::customers-by-name]]',
        params: {'min': 10},
      );
      // `ref` is the documented wire key; the wikilink form is accepted
      // and normalised server-side.
      expect(args!['ref'], '[[query::customers-by-name]]');
      expect(args!['params'], {'min': 10});
      expect(r.rows.single['total'], 50);
      expect(r.columns.map((c) => c.name).toList(), ['name', 'total']);
      expect(r.columns.first.dartType, 'VARCHAR');
      expect(r.truncated, isTrue);
    });

    test(
      'create_remote_instance sends skill/id and returns the page id',
      () async {
        Map<String, dynamic>? args;
        mock.toolHandlers['create_remote_instance'] = (a) {
          args = a;
          return {
            'page_id': 'markdown/instances/quote/aapl.md',
            'kind': 'openapi',
            'endpoint': 'yahoo_finance',
          };
        };
        final pageId = await client.createRemoteInstance(
          skill: 'quote',
          id: 'aapl',
          overlayBody: '# AAPL\n',
        );
        expect(pageId, 'markdown/instances/quote/aapl.md');
        expect(args!['skill'], 'quote');
        expect(args!['id'], 'aapl');
        expect(args!['overlay_body'], '# AAPL\n');
      },
    );

    test('create_remote_instance omits an absent overlay_body', () async {
      Map<String, dynamic>? args;
      mock.toolHandlers['create_remote_instance'] = (a) {
        args = a;
        return {'page_id': 'markdown/instances/quote/aapl.md'};
      };
      await client.createRemoteInstance(skill: 'quote', id: 'aapl');
      expect(args!.containsKey('overlay_body'), isFalse);
    });

    test(
      'register_endpoint sends kind/base_url and the optional secret',
      () async {
        Map<String, dynamic>? args;
        mock.toolHandlers['register_endpoint'] = (a) {
          args = a;
          return {'ok': true, 'name': 'yahoo_finance'};
        };
        await client.registerEndpoint(
          name: 'yahoo_finance',
          kind: 'openapi',
          baseUrl: 'https://query1.finance.yahoo.com',
          auth: 'bearer',
          secret: 's3cr3t',
        );
        expect(args!['name'], 'yahoo_finance');
        expect(args!['kind'], 'openapi');
        expect(args!['base_url'], 'https://query1.finance.yahoo.com');
        expect(args!['auth'], 'bearer');
        expect(args!['secret'], 's3cr3t');
        expect(args!.containsKey('auth_header'), isFalse);
      },
    );

    test('list_endpoints parses without a secret field', () async {
      mock.toolHandlers['list_endpoints'] = (_) => {
        'endpoints': [
          {
            'name': 'yahoo_finance',
            'kind': 'openapi',
            'base_url': 'https://query1.finance.yahoo.com',
            'auth_scheme': 'none',
            'created_by': 'admin',
          },
        ],
      };
      final eps = await client.listEndpoints();
      expect(eps.single.name, 'yahoo_finance');
      expect(eps.single.kind, 'openapi');
      expect(eps.single.baseUrl, 'https://query1.finance.yahoo.com');
      expect(eps.single.authScheme, 'none');
    });

    test('delete_endpoint sends the name', () async {
      Map<String, dynamic>? args;
      mock.toolHandlers['delete_endpoint'] = (a) {
        args = a;
        return {'ok': true};
      };
      await client.deleteEndpoint('yahoo_finance');
      expect(args!['name'], 'yahoo_finance');
    });

    test('validate_endpoints parses per-endpoint reachability', () async {
      mock.toolHandlers['validate_endpoints'] = (_) => {
        'ok': false,
        'unreachable': 1,
        'endpoints': [
          {'name': 'up', 'kind': 'mcp', 'status': 'ok'},
          {
            'name': 'down',
            'kind': 'openapi',
            'status': 'unreachable',
            'detail': 'transport error: connect refused',
          },
        ],
      };
      final health = await client.validateEndpoints();
      expect(health, hasLength(2));
      expect(health.first.healthy, isTrue);
      expect(health.last.healthy, isFalse);
      expect(health.last.detail, contains('refused'));
    });

    test(
      'ingestUpload posts base64 to /ingest/upload and parses the outcome',
      () async {
        mock.routeHandlers['/ingest/upload'] = () => {
          'status': 'materialised',
          'event_id': 'e1',
          'page_id': 'markdown/instances/memo/doc-abc.md',
          'handler_skill': 'memo',
          'chunk_count': 3,
        };
        final out = await client.ingestUpload(
          contentType: 'text/plain',
          bytes: 'hello'.codeUnits,
        );
        expect(out.materialised, isTrue);
        expect(out.chunkCount, 3);
        expect(out.pageId, contains('memo/doc-'));
      },
    );
  });
}
