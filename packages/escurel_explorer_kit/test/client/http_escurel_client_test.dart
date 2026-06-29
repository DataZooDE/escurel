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

    test('version tolerates a plain-text body (escurel returns a bare string)',
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
