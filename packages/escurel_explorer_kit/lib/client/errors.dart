/// Errors returned by [EscurelClient] implementations.
///
/// All client errors derive from [EscurelClientException]. The two
/// subtypes distinguish between *transport* failures (the server
/// could not be reached, the request was malformed at the wire
/// level) and *tool* failures (the server processed the call and
/// returned an error envelope per the MCP contract).
library;

sealed class EscurelClientException implements Exception {
  const EscurelClientException(this.message);
  final String message;

  @override
  String toString() => '$runtimeType: $message';
}

/// The server could not be reached or the response was unintelligible.
class EscurelTransportException extends EscurelClientException {
  const EscurelTransportException(super.message, {this.cause});
  final Object? cause;
}

/// The server returned a tool-level error envelope.
///
/// Newer gateways attach a machine-readable `error.data` object —
/// `{code, retryable, ...}` with stable string codes
/// (`quota_exhausted`, `read_only_replica`, …). When present, [code]
/// carries that stable code and [retryable] its retry hint; on older
/// gateways [code] falls back to the numeric JSON-RPC code and
/// [retryable] stays false. [details] is the raw `data` map (extra
/// fields like `dimension` / `retry_after_ms` live there).
class EscurelToolException extends EscurelClientException {
  const EscurelToolException(
    super.message, {
    required this.code,
    this.retryable = false,
    this.details,
  });

  /// Parse a JSON-RPC `error` object into the exception, preferring
  /// the stable string code in `error.data.code` over the numeric
  /// JSON-RPC code (older gateways carry no `data`).
  factory EscurelToolException.fromJsonRpcError(Map err) {
    final data = err['data'] is Map
        ? (err['data'] as Map).cast<String, Object?>()
        : null;
    final stableCode = data?['code'];
    return EscurelToolException(
      (err['message'] as String?) ?? 'tool error',
      code: stableCode is String && stableCode.isNotEmpty
          ? stableCode
          : (err['code']?.toString()) ?? 'unknown',
      retryable: data?['retryable'] == true,
      details: data,
    );
  }

  final String code;

  /// Whether the server marked the failure safe to retry (e.g.
  /// `read_only_replica` — retry against the writer).
  final bool retryable;

  final Map<String, Object?>? details;

  /// `retry_after_ms` from the error data (`quota_exhausted`), or null.
  int? get retryAfterMs => (details?['retry_after_ms'] as num?)?.toInt();

  /// `dimension` from the error data (`quota_exhausted`), or null.
  String? get dimension => details?['dimension'] as String?;
}

/// A human-readable line for any client error — branches on the stable
/// machine code when the gateway sent one, and falls back to the raw
/// message otherwise (older gateways carry no `error.data`). The single
/// place every status line / error block routes through.
String humanizeEscurelError(Object error) {
  if (error is EscurelToolException) {
    switch (error.code) {
      case 'admin_required':
        return 'Admin role required for this operation.';
      case 'tenant_suspended':
        return 'This tenant is suspended — contact your operator.';
      case 'forbidden':
        return 'You do not have permission to do that.';
      case 'failed_precondition':
        return 'The operation cannot run in the current state. '
            '(${error.message})';
      case 'read_only_replica':
        return 'This replica is read-only — writes go to the writer.';
      case 'unsupported_on_replica':
        return 'Not supported on a read replica — use the writer.';
      case 'publish_unavailable':
        return 'Publishing is unavailable right now — try again later.';
      case 'quota_exhausted':
        final ms = error.retryAfterMs;
        final dim = error.dimension;
        final what = dim == null ? 'Rate limit' : 'Rate limit ($dim)';
        return ms == null
            ? '$what reached — try again later.'
            : '$what reached — retry in ${(ms / 1000).ceil()}s.';
      case 'layer_read_only':
        return 'This page is on a read-only base layer (imported pack).';
      case 'session_cap_reached':
        return 'Too many concurrent sessions — close one and retry.';
      case 'unknown_session':
        return 'This editing session is no longer valid — reopen the page.';
      case 'event_not_found':
        return 'That event no longer exists.';
      case 'already_assigned':
        return 'This event is already assigned to an instance.';
    }
    return error.message;
  }
  if (error is EscurelClientException) return error.message;
  return '$error';
}

/// The client was asked for a capability the backend does not (yet)
/// expose. Surfaces in fixture mode when a tool isn't seeded, or
/// against an early-milestone server reporting a feature gap via
/// `/version`.
class EscurelUnsupportedException extends EscurelClientException {
  const EscurelUnsupportedException(super.message);
}
