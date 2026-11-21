# syntax=docker/dockerfile:1

FROM rust:1-slim AS builder

ARG FEATURES=""

WORKDIR /app

COPY . .

RUN OFFLINE=""; \
    if [ -d vendor ]; then OFFLINE="--offline"; fi; \
    if [ -n "$FEATURES" ]; then \
        cargo build --release --locked $OFFLINE --features "$FEATURES"; \
    else \
        cargo build --release --locked $OFFLINE; \
    fi \
    && strip /app/target/release/web-vault || true

FROM debian:trixie-slim AS runtime

ARG FEATURES=""

# Reuse ca-certificates from builder (rust:1-slim ships Debian trixie-slim with ca-certificates pre-installed)
COPY --from=builder /etc/ssl/certs /etc/ssl/certs
COPY --from=builder /usr/share/ca-certificates /usr/share/ca-certificates

RUN case ",$FEATURES," in \
      *,chrome,*) apt-get update \
          && apt-get install -y --no-install-recommends ca-certificates chromium \
          && rm -rf /var/lib/apt/lists/* ;; \
    esac

RUN useradd --system --create-home --home-dir /home/webvault --shell /usr/sbin/nologin webvault \
    && mkdir -p /data /sources \
    && chown -R webvault:webvault /data

COPY --from=builder /app/target/release/web-vault /usr/local/bin/web-vault

USER webvault
ENV WEB_VAULT_HOME=/data
VOLUME ["/data"]
EXPOSE 4317

ENTRYPOINT ["web-vault", "serve", "--bind", "0.0.0.0:4317"]
