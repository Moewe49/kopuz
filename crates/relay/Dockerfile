# Build from the repository root:
#   docker build -t kopuz-relay -f crates/relay/Dockerfile .
#
# Only the relay is built. `--features server` brings in axum; reqwest and the
# whole of the music app stay out of the image.

FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p relay --features server --bin kopuz-relay

# Distroless rather than a shell image: this program opens a socket that may be
# reachable from outside, and there is nothing here for anyone who gets in --
# no shell, no package manager, no coreutils. It costs nothing, since a
# statically-sensible Rust binary needs none of them.
FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/kopuz-relay /usr/local/bin/kopuz-relay

# Somewhere writable that survives the container. Mount a volume here.
ENV KOPUZ_RELAY_DATA=/data/state.json
ENV KOPUZ_RELAY_BIND=0.0.0.0:8484
VOLUME ["/data"]
EXPOSE 8484

# Not root. The relay needs no privilege beyond binding a high port and writing
# one file.
USER nonroot:nonroot

ENTRYPOINT ["/usr/local/bin/kopuz-relay"]
