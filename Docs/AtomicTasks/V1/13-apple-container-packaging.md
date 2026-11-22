# Task 13: Apple `container` Packaging (`Dockerfile` & `Makefile`)

- **Phase:** 2 (Full V1 — completes V1!)
- **Dependencies:** [12-serve-and-mcp-http.md](./12-serve-and-mcp-http.md)
- **Suggested Commit:** `feat(container): add multi-stage Dockerfile and Apple container Makefile`

## Goal
Package `citadel` for default deployment using Apple's **`container`** CLI (`https://github.com/apple/container`) per [Docs/SPEC.md](../../SPEC.md#L434-L459): a minimal multi-stage OCI `Dockerfile` (non-root user, ~25–35 MB image, optional `--build-arg FEATURES=chrome`), `.dockerignore`, and a `Makefile` wrapping `container` commands (`build`, `run`, `stop`, `logs`, `exec`, `clean`).

## Exact Files
- `Dockerfile`
- `.dockerignore`
- `Makefile`
- `README.md` (concise quickstart for Apple `container` default + native install + MCP client config)

## Red-First / Verification Checks
1. Verify `Dockerfile` builds a static/slim non-root image with `CITADEL_HOME=/data`, `EXPOSE 4317`, and `ENTRYPOINT ["citadel", "serve", "--bind", "0.0.0.0:4317"]`.
2. Verify `Makefile` uses strictly Apple's `container` CLI (`container system start`, `container volume create`, `container build`, `container run`, `container exec`, `container logs`, `container stop`) — zero `docker` invocations.
3. Run the `AGENTS.md` container build gate (`container build -t citadel:check .`) and smoke-test `/api/health` + `/mcp` against a running container.

## Acceptance Criteria
- Multi-stage `Dockerfile` (`rust:1-slim` builder -> `debian:bookworm-slim` runtime, non-root `citadel` user, `ca-certificates` included for HTTPS crawling).
- `Makefile` supports overriding `BOOKS_DIR` (optional `:ro` bind mount to `/sources/books`) and `PORT` (default `4317`, bound to `127.0.0.1:4317:4317`).
- `README.md` documents: Apple `container` quickstart, native `cargo install --path .` + `daemon install`, Claude/Antigravity MCP config (`http://127.0.0.1:4317/mcp` and `stdio`), and `citadel skill install`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
container build -t citadel:check .
```
