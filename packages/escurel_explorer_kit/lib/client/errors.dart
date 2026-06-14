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
class EscurelToolException extends EscurelClientException {
  const EscurelToolException(super.message, {required this.code, this.details});

  final String code;
  final Map<String, Object?>? details;
}

/// The client was asked for a capability the backend does not (yet)
/// expose. Surfaces in fixture mode when a tool isn't seeded, or
/// against an early-milestone server reporting a feature gap via
/// `/version`.
class EscurelUnsupportedException extends EscurelClientException {
  const EscurelUnsupportedException(super.message);
}
