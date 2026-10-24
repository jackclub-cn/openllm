# Builds the single-binary gateway with the console embedded.
#
# Stages:
#   web       frontend build (must run first: `rust-embed` bakes `web/dist` into
#             the binary at compile time and build.rs fails a release build when
#             it is missing)
#   rustbase  shared Rust toolchain + release-profile settings
#   deps      compiles every third-party crate once, against stub sources
#   build     compiles the real gateway, reusing `deps`' target directory
#   runtime   distroless-ish slim image, non-root, single binary
#
# The `deps` stage exists because a cold release build of this crate's
# dependency tree (tokio + axum + reqwest + sqlx) dominates image build time and
# everything above it changes on nearly every commit. Cargo fingerprints
# dependencies from the profile settings, so `deps` and `build` MUST agree on
# the release profile — that is why both inherit `rustbase`.
#
# Release profile is taken from `[profile.release]` in Cargo.toml by default.
# A faster, unoptimised CI check can override it consistently in both stages:
#   docker build --build-arg OPENLLM_LTO=false \
#                --build-arg OPENLLM_CODEGEN_UNITS=16 \
#                --build-arg OPENLLM_OPT_LEVEL=1 \
#                --build-arg OPENLLM_BUILD_JOBS=4 -t openllm:ci .
FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:bookworm AS rustbase
ARG OPENLLM_LTO=thin
ARG OPENLLM_CODEGEN_UNITS=1
ARG OPENLLM_OPT_LEVEL=s
# Cargo's own parallelism. `default` uses every CPU; lower it on
# memory-constrained builders (Docker Desktop VMs are often handed 4 CPUs and
# little RAM) to keep rustc from swapping or killing the builder mid-build.
ARG OPENLLM_BUILD_JOBS=default
ENV CARGO_PROFILE_RELEASE_LTO=${OPENLLM_LTO} \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=${OPENLLM_CODEGEN_UNITS} \
    CARGO_PROFILE_RELEASE_OPT_LEVEL=${OPENLLM_OPT_LEVEL} \
    CARGO_BUILD_JOBS=${OPENLLM_BUILD_JOBS} \
    CARGO_TERM_COLOR=never

FROM rustbase AS deps
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
# build.rs refuses a release build without an embedded console, so write a stub
# dist plus stub crate sources: this stage is only about warming the cache.
RUN mkdir -p web/dist src \
    && printf 'stub\n' > web/dist/index.html \
    && printf 'fn main() {}\n' > src/main.rs \
    && : > src/lib.rs \
    && cargo build --release --locked

FROM rustbase AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY migrations ./migrations
# Reuse the warm dependency graph; only the gateway crate itself recompiles.
COPY --from=deps /src/target ./target
COPY src ./src
# The built console must exist before the release build embeds it.
COPY --from=web /src/web/dist ./web/dist
# `COPY` preserves the source mtimes from the build context, so the real
# sources can look *older* than the stub-built artifact that `deps` just
# produced. Cargo would then consider the crate fresh and link the stub
# `fn main() {}` into the final image. Drop the crate's own fingerprint and
# touch its sources so it is unconditionally rebuilt; dependencies stay cached.
RUN rm -rf target/release/.fingerprint/openllm-* target/release/deps/openllm-* \
    && find src -type f -name '*.rs' -exec touch {} + \
    && cargo build --release --locked \
    && cp target/release/openllm /src/openllm

FROM debian:bookworm-slim AS runtime

# curl backs the container health check; ca-certificates and tzdata are needed
# for TLS upstreams and for the console's local-time display.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tzdata \
    && rm -rf /var/lib/apt/lists/*

# The gateway never needs root: it only writes its own data directory.
RUN groupadd --gid 10001 openllm \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /data --shell /usr/sbin/nologin openllm \
    && install -d -o openllm -g openllm -m 0750 /data

COPY --from=build /src/openllm /usr/local/bin/openllm

# Bind on all interfaces so the container is reachable, and keep state in the
# volume declared below.
ENV OPENLLM_BIND=0.0.0.0:8080 \
    OPENLLM_DATA_DIR=/data \
    MALLOC_ARENA_MAX=2

VOLUME ["/data"]
EXPOSE 8080

USER openllm
WORKDIR /data

# /api/ready (not /api/health) on purpose: it turns 503 while the instance is
# draining, so `docker compose up -d` and Swarm/orchestrator rollouts wait for a
# gateway that can actually serve and stop routing to one that is shutting down.
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/api/ready || exit 1

ENTRYPOINT ["/usr/local/bin/openllm"]
