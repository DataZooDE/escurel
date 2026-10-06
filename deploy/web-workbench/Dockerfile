# Escurel web workbench: VS Code in the browser (code-server) with the Escurel extension installed,
# the calm "focus" layout preset, and the developer surface cut away.
#
#   docker build -f deploy/web-workbench/Dockerfile -t escurel-workbench .      # context = repo root
#
# See docs/deploy/web-workbench.md for the security model. Short version: one shared password,
# TLS from a reverse proxy, no terminal (the pty module is removed), no marketplace, no uploads or
# downloads, non-root, and meant to run with a read-only root filesystem and no capabilities.

# ---- the extension, as a .vsix --------------------------------------------------------------------
FROM node:22-bookworm-slim AS vsix
WORKDIR /src/editors/vscode
COPY editors/vscode/package.json editors/vscode/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY editors/vscode/ ./
# `npm run package` = build + `vsce package`. Not a release: nothing is tagged or uploaded.
RUN npm run package && mv escurel-*.vsix /escurel.vsix
# The tiny bootstrap (opens the views and the Overview board on load; carries no credential).
COPY deploy/web-workbench/bootstrap /bootstrap
WORKDIR /bootstrap
RUN /src/editors/vscode/node_modules/.bin/vsce package --allow-missing-repository --skip-license \
      -o /escurel-web-bootstrap.vsix

# ---- the login page must be the Calm theme's colours: fail the build if login/global.css has drifted -----
FROM node:22-bookworm-slim AS logincheck
COPY editors/vscode/themes /repo/editors/vscode/themes
COPY deploy/web-workbench/login /repo/deploy/web-workbench/login
RUN node /repo/deploy/web-workbench/login/gen-global-css.mjs --check && touch /login-in-sync

# ---- the workbench ----------------------------------------------------------------------------------
# Pinned. The extension needs VS Code >= 1.104 (engines.vscode); code-server 4.140.0 carries 1.140.0.
FROM codercom/code-server:4.140.0
USER root

# Cut the developer surface. Each of these is either a way to run a process (the terminal's pty, debuggers,
# task providers, git), a sign-in to somebody else's service, or an AI assistant.
#  - node-pty: the integrated terminal and every task run through it. Without the native module the
#    pty host cannot spawn anything: this is the block that does not depend on a setting a person can edit.
#  - sudo: the base image gives `coder` passwordless sudo.
RUN set -eux; \
    cd /usr/lib/code-server/lib/vscode; \
    rm -rf node_modules/node-pty/build node_modules/node-pty/prebuilds; \
    cd extensions; \
    rm -rf copilot git github github-authentication microsoft-authentication \
           ms-vscode.js-debug ms-vscode.js-debug-companion ms-vscode.vscode-js-profile-table \
           debug-auto-launch debug-server-ready npm grunt gulp jake merge-conflict; \
    rm -f /etc/sudoers /etc/sudoers.d/* /usr/bin/sudo /usr/bin/fixuid /usr/local/bin/fixuid; \
    find / -xdev -perm /6000 -type f -exec chmod a-s {} + 2>/dev/null || true

COPY --from=vsix /escurel.vsix /escurel-web-bootstrap.vsix /tmp/
COPY --from=logincheck /login-in-sync /tmp/login-in-sync
COPY deploy/web-workbench/ /opt/escurel/
# Extensions live in the image, not in a volume: what is installed is what was built.
RUN set -eux; mkdir -p /opt/escurel/extensions /opt/escurel/ud; \
    HOME=/tmp /usr/bin/code-server --user-data-dir /opt/escurel/ud --extensions-dir /opt/escurel/extensions \
      --install-extension /tmp/escurel.vsix --install-extension /tmp/escurel-web-bootstrap.vsix; \
    HOME=/tmp /usr/bin/code-server --user-data-dir /opt/escurel/ud --extensions-dir /opt/escurel/extensions --list-extensions --show-versions \
      | grep -q '^datazoo.escurel@'; \
    rm -rf /opt/escurel/ud /tmp/*.vsix /tmp/login-in-sync /opt/escurel/bootstrap /opt/escurel/Dockerfile* \
    && chown -R root:root /opt/escurel && chmod -R a+rX,go-w /opt/escurel \
    && chmod +x /opt/escurel/entrypoint.sh

# The Escurel skin for code-server's own pages (login, error) and its icons. These are template/asset files of
# the pinned code-server version: assert they exist first, so an upgrade that moves them fails the build instead
# of silently shipping the stock page. The login template keeps every placeholder the server fills in
# ({{ERROR}}, {{BASE}}, {{WELCOME_TEXT}}, ...) and the form's field names, so login itself is unchanged.
RUN set -eux; \
    SRC=/usr/lib/code-server/src/browser; \
    for f in pages/login.html pages/login.css pages/global.css pages/error.css \
             media/favicon.svg media/favicon-dark-support.svg media/favicon.ico \
             media/pwa-icon-192.png media/pwa-icon-512.png media/pwa-icon-maskable-192.png media/pwa-icon-maskable-512.png; do \
      test -f "$SRC/$f"; \
    done; \
    cd /opt/escurel/login; \
    install -m 0644 login.html login.css global.css error.css "$SRC/pages/"; \
    install -m 0644 favicon.svg favicon-dark-support.svg favicon.ico pwa-icon-192.png pwa-icon-512.png \
                    pwa-icon-maskable-192.png pwa-icon-maskable-512.png "$SRC/media/"; \
    grep -q '{{ERROR}}' "$SRC/pages/login.html" && grep -q 'name="password"' "$SRC/pages/login.html"

# The data directory exists and belongs to `coder` BEFORE it becomes a volume, so a fresh named volume
# inherits that ownership (a root-owned volume would stop the non-root process from starting).
RUN mkdir -p /home/coder/data /home/coder/workspace && chown coder:coder /home/coder/data /home/coder/workspace

USER coder
ENV EXTENSIONS_GALLERY="{}" \
    PORT=8080 \
    WORKBENCH_DATA_DIR=/home/coder/data
EXPOSE 8080
VOLUME ["/home/coder/data"]
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=5 \
  CMD curl -fsS "http://127.0.0.1:${PORT}/healthz" >/dev/null || exit 1
ENTRYPOINT ["/opt/escurel/entrypoint.sh"]
