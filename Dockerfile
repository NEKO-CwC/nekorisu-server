# nekorisu-server, one image: Node builds the panel, Rust builds the server,
# and the final stage holds only the binary.
#
# The server depends on the client repository (the signing library and
# cloud.proto), which is not published yet, so it comes in as a second build
# context:
#
#     docker build --build-context nekorisu=../nekorisu -t nekorisu-server .
#
# Build with --build-arg NEKORISU_COMMIT=$(git rev-parse --short=7 HEAD) to put
# the commit in the panel's footer; there is no .git inside the build.

FROM node:22-alpine AS web
WORKDIR /web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM rust:1-bookworm AS build
# crates/cloud reaches the client at ../../../nekorisu, which is /nekorisu here.
# Only what its package needs, not its target/ or .git.
COPY --from=nekorisu Cargo.toml build.rs /nekorisu/
COPY --from=nekorisu src /nekorisu/src
COPY --from=nekorisu proto /nekorisu/proto
COPY --from=nekorisu keys /nekorisu/keys
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY --from=web /web/dist web/dist
ARG NEKORISU_COMMIT=unknown
ENV NEKORISU_COMMIT=${NEKORISU_COMMIT}
RUN cargo build --release --locked -p nekorisu-server \
 && mkdir -p /rootfs/data /rootfs/config \
 && cp target/release/nekorisu-server /rootfs/nekorisu-server

FROM gcr.io/distroless/cc-debian12:nonroot
# 65532 is the image's non-root user. A named volume mounted on /data gets this
# ownership; a bind mount must be made writable for that user.
COPY --from=build --chown=65532:65532 /rootfs/data /data
COPY --from=build --chown=65532:65532 /rootfs/config /config
COPY --from=build /rootfs/nekorisu-server /usr/local/bin/nekorisu-server
ENV NEKORISU_DATA_DIR=/data \
    NEKORISU_CONFIG_DIR=/config \
    NEKORISU_HTTP_ADDR=0.0.0.0:8080 \
    NEKORISU_GRPC_ADDR=0.0.0.0:7070
VOLUME ["/data"]
EXPOSE 8080 7070
USER nonroot
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD ["/usr/local/bin/nekorisu-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/nekorisu-server"]
