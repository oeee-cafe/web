# Build Rust binary
FROM rust:1.98 AS rust-builder
WORKDIR /app

# Install sccache prebuilt binary
RUN case "$(uname -m)" in \
    x86_64) ARCH=x86_64 ;; \
    aarch64) ARCH=aarch64 ;; \
    *) echo "Unsupported architecture: $(uname -m)" && exit 1 ;; \
    esac && \
    curl -L "https://github.com/mozilla/sccache/releases/download/v0.10.0/sccache-v0.10.0-${ARCH}-unknown-linux-musl.tar.gz" | \
    tar -xz --strip-components=1 -C /usr/local/bin/

# Copy source files
# build.rs is what makes a migrations-only change rebuild the binary; without it
# here, cargo finds no build script and silently skips the dependency tracking.
COPY Cargo.toml Cargo.lock build.rs ./
COPY locales/ ./locales/
COPY migrations/ ./migrations/
COPY src/ ./src/

# Build with sccache
ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache
ENV SCCACHE_CACHE_SIZE="10G"
ENV DATABASE_URL=postgresql://postgres:postgres@host.docker.internal:5433/oeee_cafe

# /app/target is a cache mount, so anything needed later must be copied out
# inside this RUN.
#
# The release profile builds full debug info, and it is split off here: the
# image gets the binary without it, and deploy.sh takes oeee-cafe.debug from
# the debug-files stage below and uploads it to Sentry, which puts file, line
# and inlined frames back into its stack traces by the GNU build id the two
# share. The symbol table stays in, so a backtrace in `docker logs` still
# names its functions.
RUN --mount=type=cache,target=/sccache \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && \
    sccache --show-stats && \
    cp /app/target/release/oeee-cafe /app/oeee-cafe && \
    objcopy --only-keep-debug --compress-debug-sections=zlib /app/oeee-cafe /app/oeee-cafe.debug && \
    objcopy --strip-debug /app/oeee-cafe

# Build neo-cucumber
FROM node:24-slim AS node-builder-neo-cucumber
WORKDIR /app/neo-cucumber
COPY neo-cucumber/package.json neo-cucumber/pnpm-lock.yaml neo-cucumber/ ./
COPY frontend/ /app/frontend/
RUN npm install --global corepack@latest
RUN corepack enable pnpm
RUN corepack use pnpm@latest-10
RUN pnpm install --frozen-lockfile
RUN pnpm run build

# `docker build --target debug-files --output type=local,dest=DIR` writes
# oeee-cafe.debug to DIR for uploading, from the same cached build as the image.
FROM scratch AS debug-files
COPY --from=rust-builder /app/oeee-cafe.debug /

# Build runtime image. The last stage, so it is what a plain `docker build` makes.
#
# A glibc at least as new as rust-builder's (bookworm, 2.36) is all the binary
# asks of it. This was ubuntu:25.10 until it was noticed that an interim
# release had gone out of support months before and was getting no security
# updates; a Debian stable does not do that without a year's warning.
FROM debian:trixie-slim
WORKDIR /app
# libssl3t64 is linked by the binary (see `ldd oeee-cafe`). It used to arrive
# only as a dependency of curl, which is what the compose healthcheck shells
# out to, so it is named here in its own right.
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl libssl3t64 && \
    rm -rf /var/lib/apt/lists/*
COPY tegaki/ ./tegaki/
COPY locales/ ./locales/
COPY static/ ./static/
COPY templates/ ./templates/
# Admin/ops commands are `./oeee-cafe cli ...`. ./cli.sh finds whichever
# blue/green colour is serving and runs them inside it, e.g.
#   ./cli.sh set-role <login_name> admin
COPY --from=rust-builder /app/oeee-cafe ./
COPY --from=node-builder-neo-cucumber /app/neo-cucumber/dist/ ./neo-cucumber/dist/
COPY --from=node-builder-neo-cucumber /app/neo-cucumber/dist-viewer/ ./neo-cucumber/dist-viewer/
COPY --from=node-builder-neo-cucumber /app/neo-cucumber/dist-offline/ ./neo-cucumber/dist-offline/
COPY --from=node-builder-neo-cucumber /app/neo-cucumber/dist-replay/ ./neo-cucumber/dist-replay/

# Versions the static asset URLs the server hands out, so a deploy invalidates
# browser and CDN caches and nothing else does. Read at runtime and declared in
# this stage on purpose: putting it in the builder would invalidate the Rust
# build cache on every deploy, and sccache does not key on it anyway, so a
# compile-time value came back stale from cache.
ARG GIT_COMMIT=""
ENV GIT_COMMIT=$GIT_COMMIT

EXPOSE 3000
CMD ["./oeee-cafe", "config/config.toml"]
