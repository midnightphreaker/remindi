FROM rust:1.97.1-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release --bin remindi

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 remindi \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent remindi \
    && install --directory --owner 10001 --group 10001 --mode 0700 /data /data/backups

COPY --from=builder --chown=10001:10001 /src/target/release/remindi /usr/local/bin/remindi

USER 10001:10001
WORKDIR /data
EXPOSE 8000
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["curl", "--fail", "--silent", "--show-error", "http://127.0.0.1:8000/health/live"]

ENTRYPOINT ["/usr/local/bin/remindi"]
