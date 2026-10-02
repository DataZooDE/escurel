# Closing a `ws` socket that is still connecting is an uncaught exception

**Symptom.** An integration test failed once with `Uncaught Error: WebSocket was closed before the
connection was established`, in a test that signed the extension in as another user. It passed on
the next run. Nothing in the test touched a socket.

**Cause.** `EventSocket` dropped a socket with `removeAllListeners(); close()`. That removes the
`error` listener too. Closing a `ws` socket that is still in its handshake makes `ws` emit
`'error'` on the next tick, and an `'error'` event with no listener is thrown as an uncaught
exception in the extension host. A token change reconnects every live socket (`reconnectNow`), so
anything signing in or out while a socket was mid-handshake could hit it.

**Fix.** One `abandon()` that removes the listeners and immediately puts a no-op `error` listener
back before closing. Two unit tests hold a socket in its handshake against a TCP server that never
answers the upgrade and assert nothing escapes, for both `close()` and a token change.

**Recognise it next time.** An uncaught `WebSocket was closed before the connection was
established` is never about the server: it is a socket being closed in `CONNECTING` with its error
listener gone. Whenever code calls `removeAllListeners()` on an emitter it is about to close, ask
what the close itself can emit.
