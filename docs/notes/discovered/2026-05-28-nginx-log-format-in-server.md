# `log_format` inside a `server{}` block is rejected by nginx

**Date:** 2026-05-28
**Scope:** `apps/escurel-explore` runtime image (nginx 1.27-alpine)

## Symptom

The freshly built escurel-explore image refused to start. nginx exited
immediately with:

```
[emerg] 7#7: "log_format" directive is not allowed here in
/etc/nginx/conf.d/default.conf:57
```

`flutter analyze` / `flutter test` / `docker build` all passed cleanly
— the bug only surfaces at container start, which is not exercised by
the `explore.yml` workflow today.

## Cause

The nginx http-context parser allows `log_format` at the `http{}`
scope only. The stock nginx image's main `nginx.conf` includes files
under `/etc/nginx/conf.d/*.conf` from inside that `http{}` block, so
each conf.d file's top level is the http scope. The previous
`apps/escurel-explore/nginx.conf` nested the `log_format` block inside
its `server{ … }`, which the parser rejects.

## Fix

Move `log_format json_combined …;` out of `server{}` and place it at
the top of `apps/escurel-explore/nginx.conf` (still inside the file,
still inside http{} when included). Keep `access_log /dev/stdout
json_combined;` inside `server{}` — the `access_log` directive *is*
valid at server scope, only the format declaration must live higher.

```nginx
log_format json_combined escape=json '{...}';

server {
  listen 8080 default_server;
  ...
  access_log /dev/stdout json_combined;
}
```

## Recognise it next time

- nginx logs `"X" directive is not allowed here` mention a file:line
  inside `conf.d/*.conf` — look at the directive at that line and
  check the nginx context table (http / server / location).
- The build pipeline passes but the runtime image crash-loops on
  Nomad with `tini`/`nginx` exiting (1). Health checks fail without
  any access logs — because logging itself failed to configure.

## Follow-up

The `explore.yml` GHA workflow currently runs `flutter test` and
`flutter analyze` but never starts the built image. A two-line smoke
step (`docker run -d -p 8080:8080 …` + `curl -fsS …/healthz`) before
the push would catch this class of bug. Tracked separately.
