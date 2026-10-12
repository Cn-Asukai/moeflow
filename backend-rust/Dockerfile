ARG RUST_IMAGE=rust:1.90-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
FROM ${RUST_IMAGE} AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target cargo build --locked --release && cp target/release/moeflow-backend /moeflow-backend

FROM ${RUNTIME_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home moeflow
COPY --from=builder /moeflow-backend /usr/local/bin/moeflow-backend
USER moeflow
WORKDIR /home/moeflow
ENV LISTEN_ADDR=0.0.0.0:8000
EXPOSE 8000
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s CMD curl -fsS http://127.0.0.1:8000/health || exit 1
ENTRYPOINT ["moeflow-backend"]
