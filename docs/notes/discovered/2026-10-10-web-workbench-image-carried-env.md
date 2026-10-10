# The web workbench image carried the operator's `.env`

**Found:** 2026-10-10, agent-crew review of the ad-hoc period (verified against the running image).

**Symptom.** `docker run --rm --entrypoint ls escurel-web-workbench -a /opt/escurel` listed `.env`,
`compose*.yaml`, `compose.override.yaml`, `probe/` and `s2d-up.sh`. `.env` held the workbench password.
Every logged-in browser user could open it with File > Open File.

**Cause.** The build context is the repository root, and `Dockerfile` ran `COPY deploy/web-workbench/ /opt/escurel/`
while `Dockerfile.dockerignore` re-included `!deploy/web-workbench`. An operator's `.env` and overrides live in that
very directory on the host, so they went into the image.

**Fix.** `Dockerfile` copies the five runtime files (`entrypoint.sh`, `render-settings.mjs`, `settings.base.json`,
`keybindings.json`, `code-server.yaml`) and the `login/` directory by name; `Dockerfile.dockerignore` drops
`.env*`, `compose*`, `*.local`, `probe/` and `s2d-*.sh`; a build step fails if any of those names survive under
`/opt/escurel`; `scripts/web-workbench-smoke.sh` asserts the same on the built image. The password was rotated and
the container now gets only an argon2 `HASHED_PASSWORD`.

**Recognise it next time.** Never `COPY` a directory that is also an operator's working directory. After any
Dockerfile change run `docker run --rm --entrypoint sh <image> -c 'find /opt/escurel -name ".env*"'`.
