# Task V4-04: Docker as the default container runtime

- **Phase:** V4
- **Dependencies:** V4-02
- **Suggested Commit:** `feat(build): make Docker the default container runtime, keep Apple container optional`

## Goal
`make build`, `make run` and friends use Docker by default; Apple `container` stays available
with `RUNTIME=container`. Checks and audit evidence come from Docker (ADR-0003).

## Exact Files
- `Makefile`: `RUNTIME ?= docker`; every target runs `$(RUNTIME) …`; Apple-only steps
  (`container system start`, crate vendoring) sit inside `ifeq ($(RUNTIME),container)`; `DNS`
  defaults to `1.1.1.1` only for Apple `container`.
- `src/update/mod.rs`: `container_cli` + `write_container_rebuild_steps` print the CLI of the
  runtime in use (`/.dockerenv` → `docker`, otherwise `container`).
- `tests/container_packaging.rs`: Docker-default Makefile and README assertions.
- `README.md`, `AGENTS.md`, `REVIEW.md`, `Docs/SPEC.md`, `Docs/PRD.md`,
  `Docs/adr/0003-docker-default-container-runtime.md`.

## Red-First Test Cases
1. `container_packaging::makefile_defaults_to_docker_and_supports_apple_container`.
2. `container_packaging::readme_documents_quickstart_native_mcp_and_skill`: documents
   `docker exec -i citadel citadel mcp` and `RUNTIME=container`.
3. `update::tests::rebuild_steps_name_the_cli_of_the_running_container_runtime`.

## Acceptance Criteria
- `make -n build run` prints only `docker` commands; `make -n build run RUNTIME=container` prints
  the Apple steps, including `container system start` and vendoring.
- `make build` and `make build-lite` build with Docker; `make run` serves `/api/health` on
  `127.0.0.1:4317`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
docker build -t citadel:check .
```
