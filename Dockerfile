# syntax=docker/dockerfile:1
#
# Airlock container image.
#
# Airlock is fundamentally a local, loopback-only daemon (see SECURITY.md). This
# image exists so the server can be distributed as an OCI artifact (MCP Registry,
# Glama) and run in container-based setups. Because the daemon binds 127.0.0.1 by
# design, run it with host networking so the host's loopback reaches it:
#
#   docker run --rm --network host -v airlock:/data ghcr.io/adamorad/airlock:latest
#
# On Linux a bearer token is required; it is written to /data/.airlock/token
# (read it with `docker exec` or from the mounted volume), or set AIRLOCK_TOKEN.

# ---- build stage (cross-compiles natively, no QEMU) ----
FROM --platform=$BUILDPLATFORM golang:1.23-alpine AS build
ARG TARGETOS
ARG TARGETARCH
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
# modernc.org/sqlite is pure Go, so CGO stays off and the binary is fully static.
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags="-s -w" -o /out/airlock .

# ---- runtime stage ----
FROM alpine:3.20
# Ownership verification for the MCP Registry — MUST match "name" in server.json.
LABEL io.modelcontextprotocol.server.name="io.github.adamorad/airlock"
LABEL org.opencontainers.image.source="https://github.com/adamorad/airlock"
LABEL org.opencontainers.image.description="Local coordination daemon for AI agents — named locks, shared state, events, and a task queue over MCP."
LABEL org.opencontainers.image.licenses="MIT"
COPY --from=build /out/airlock /usr/local/bin/airlock
# State DB and token file live under /data; mount a volume to persist them.
ENV HOME=/data
ENV AIRLOCK_DB=/data/state.db
RUN mkdir -p /data
VOLUME ["/data"]
EXPOSE 27183
ENTRYPOINT ["airlock", "daemon"]
