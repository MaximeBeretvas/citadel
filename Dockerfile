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
    && strip /app/target/release/citadel || true

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

RUN useradd --system --create-home --home-dir /home/citadel --shell /usr/sbin/nologin citadel \
    && mkdir -p /data /sources \
    && chown -R citadel:citadel /data

COPY --from=builder /app/target/release/citadel /usr/local/bin/citadel

USER citadel
ENV CITADEL_HOME=/data
VOLUME ["/data"]
EXPOSE 4317

ENTRYPOINT ["citadel", "serve", "--bind", "0.0.0.0:4317"]
