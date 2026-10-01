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
RUN --mount=type=cache,id=libgendex-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=libgendex-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=libgendex-target-${TARGETARCH}-${FEATURES},target=/src/target \
    if [ "$TARGETARCH" = "amd64" ] || [ -z "$TARGETARCH" ]; then export RUSTFLAGS="-C target-cpu=$TARGET_CPU"; fi \
    && cargo build --release --locked -p libgendex --features "$FEATURES" \
    && install -Dm755 target/release/libgendex /out/libgendex \
    && /out/libgendex openapi > /out/openapi.json

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
    && groupadd --gid 1000 libgendex \
    && useradd --system --uid 1000 --gid 1000 --home /app libgendex \
    && mkdir -p /data /library && chown libgendex /data /library
ARG VERSION=0.2.0
ARG REVISION=unknown
ARG SOURCE=https://github.com/wexder/libgendex
LABEL org.opencontainers.image.title="libgendex" \
      org.opencontainers.image.description="Native Rust Library Genesis catalogue search" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.revision="$REVISION" \
      org.opencontainers.image.source="$SOURCE"
WORKDIR /app
COPY --from=server /out/libgendex /usr/local/bin/libgendex
COPY --from=web /src/web/dist /app/web
ENV LIBGENDEX_SERVER__BIND=0.0.0.0:8080 \
    LIBGENDEX_SERVER__STATIC_DIR=/app/web \
    LIBGENDEX_PATHS__DATA_DIR=/data \
    LIBGENDEX_PATHS__LIBRARY_DIR=/library \
    LIBGENDEX_CONFIG=/app/libgendex.toml
USER 1000:1000
VOLUME ["/data", "/library"]
EXPOSE 8080
ENTRYPOINT ["libgendex"]
