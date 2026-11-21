# syntax=docker/dockerfile:1

FROM rust:1-slim AS builder

ARG FEATURES=""

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN if [ -n "$FEATURES" ]; then \
        cargo build --release --locked --features "$FEATURES"; \
    else \
        cargo build --release --locked; \
    fi \
    && strip /app/target/release/web-vault || true

FROM debian:bookworm-slim AS runtime

ARG FEATURES=""

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && case ",$FEATURES," in \
         *,chrome,*) apt-get install -y --no-install-recommends chromium ;; \
       esac \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --home-dir /home/webvault --shell /usr/sbin/nologin webvault \
    && mkdir -p /data /sources \
    && chown -R webvault:webvault /data

COPY --from=builder /app/target/release/web-vault /usr/local/bin/web-vault

USER webvault
ENV WEB_VAULT_HOME=/data
VOLUME ["/data"]
EXPOSE 4317

ENTRYPOINT ["web-vault", "serve", "--bind", "0.0.0.0:4317"]
