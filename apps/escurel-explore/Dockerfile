# syntax=docker/dockerfile:1.7
#
# Multi-stage build: Flutter SDK → flutter build web → nginx:alpine.
#
# Resulting image: ~30 MiB. Listens on :8080 per substrate convention.
# /healthz returns 200 ok — dependency-free liveness for Consul.
#
# Build:
#   docker build -t escurel-explore:local \
#     --build-arg BUILD_VERSION=$(git rev-parse --short HEAD) \
#     apps/escurel-explore
#
# Run locally (fixture mode):
#   docker run --rm -p 8080:8080 escurel-explore:local
#   curl localhost:8080/healthz
#   open http://localhost:8080

FROM ghcr.io/cirruslabs/flutter:3.44.0 AS build

WORKDIR /src
COPY pubspec.yaml pubspec.lock ./
RUN flutter pub get

COPY . .

ARG BUILD_VERSION=dev
ARG ESCUREL_EXPLORE_MODE=fixture
RUN flutter build web --release \
      --dart-define=ESCUREL_EXPLORE_MODE=${ESCUREL_EXPLORE_MODE} \
      --dart-define=ESCUREL_EXPLORE_VERSION=${BUILD_VERSION}

# --- runtime ----------------------------------------------------

FROM nginx:1.27-alpine AS runtime

RUN apk add --no-cache curl tini

COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /src/build/web /usr/share/nginx/html

# Substrate liveness contract — dependency-free, fast, no caching.
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://localhost:8080/healthz || exit 1

EXPOSE 8080

ENTRYPOINT ["/sbin/tini", "--", "nginx", "-g", "daemon off;"]
