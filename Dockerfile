# syntax=docker/dockerfile:1

FROM rust:1-slim-trixie AS server
ARG RUST_VERSION=1.99.0
ARG FEATURES=""
# Portable baseline for published amd64 images; local builds can opt into x86-64-v3.
ARG TARGET_CPU=x86-64
ARG TARGETARCH
RUN rustup toolchain install "$RUST_VERSION" --profile minimal \
    && rustup default "$RUST_VERSION" \
    && apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake pkg-config \
    && if [ "$FEATURES" = "local" ]; then apt-get install -y --no-install-recommends clang libclang-dev; fi \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/myisam-reader ./crates/myisam-reader
COPY src ./src
RUN --mount=type=cache,id=bookjev-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=bookjev-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=bookjev-target-${TARGETARCH}-${FEATURES},target=/src/target \
    if [ "$TARGETARCH" = "amd64" ] || [ -z "$TARGETARCH" ]; then export RUSTFLAGS="-C target-cpu=$TARGET_CPU"; fi \
    && cargo build --release --locked -p bookjev --features "$FEATURES" \
    && install -Dm755 target/release/bookjev /out/bookjev \
    && /out/bookjev openapi > /out/openapi.json

# The frontend is static output, so its build tools can run on the builder's CPU.
FROM --platform=$BUILDPLATFORM node:22-trixie-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web ./
COPY --from=server /out/openapi.json /src/openapi.json
RUN npm run build

FROM debian:trixie-slim
ARG FEATURES=""
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && if [ "$FEATURES" = "local" ]; then apt-get install -y --no-install-recommends libgomp1 libstdc++6; fi \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 bookjev \
    && useradd --system --uid 1000 --gid 1000 --home /app bookjev \
    && mkdir -p /data /library && chown bookjev /data /library
ARG VERSION=0.1.0
ARG REVISION=unknown
ARG SOURCE=https://github.com/wexder/libgendex
LABEL org.opencontainers.image.title="bookjev" \
      org.opencontainers.image.description="Native Rust Library Genesis catalogue search" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.revision="$REVISION" \
      org.opencontainers.image.source="$SOURCE"
WORKDIR /app
COPY --from=server /out/bookjev /usr/local/bin/bookjev
COPY --from=web /src/web/dist /app/web
ENV BOOKJEV_SERVER__BIND=0.0.0.0:8080 \
    BOOKJEV_SERVER__STATIC_DIR=/app/web \
    BOOKJEV_PATHS__DATA_DIR=/data \
    BOOKJEV_PATHS__LIBRARY_DIR=/library \
    BOOKJEV_CONFIG=/app/bookjev.toml
USER 1000:1000
VOLUME ["/data", "/library"]
EXPOSE 8080
ENTRYPOINT ["bookjev"]
