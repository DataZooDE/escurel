# Kamal refuses a prebuilt image without a matching `service` label

**Symptom.** Deploying `dz-escurel-explore` on the substrate failed at
`kamal deploy` on all three hosts — the image pulled fine, then:

```
docker stdout: Image ghcr.io/datazoode/escurel-explore:sha-75e0903 is
missing the 'service' label
```

**Cause.** Kamal runs `docker inspect -f '{{ .Config.Labels.service }}'`
on the prebuilt image and aborts unless it equals the `service:` name in
the app's `kamal/<app>/deploy.yml`. When the `escurel-explore` image was
reworked from the nginx base to the Rust BFF multi-stage build, the
runtime stage didn't carry a `LABEL service=...`, so the check failed.
(The substrate skill calls this out: ref 01 — *"the image must carry
`LABEL service="<kamal-service>"` matching the Kamal service name —
Kamal refuses to boot a prebuilt image whose `service` label doesn't
match."*)

**Fix.** Add to the runtime stage of `apps/escurel-explore/Dockerfile`:

```dockerfile
LABEL service="dz-escurel-explore"
```

The value must equal `service:` in
`hetzner-agent-substrate/kamal/dz-escurel-explore/deploy.yml`.

**How to recognise it next time.** Any `--skip-push` (externally built)
substrate app whose deploy dies with *"missing the 'service' label"* — the
image's `LABEL service` is absent or doesn't match the Kamal service name.
Check it locally with
`docker inspect -f '{{ .Config.Labels.service }}' <image>`.
