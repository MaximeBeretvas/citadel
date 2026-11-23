# Docker is the default container runtime; Apple `container` is optional

The container deployment and the container checks use Docker by default. The `Makefile` takes `RUNTIME ?= docker`, and every target runs `$(RUNTIME) build|run|stop|logs|exec|rm|volume`. Apple's `container` CLI remains available with `RUNTIME=container`, which adds the two steps only it needs: `container system start` and vendoring crates for its builder VM. The AGENTS.md checks gate builds the image with `docker build`.

Apple `container` was the original default (PRD, SPEC §8.1) because it needs no Docker Desktop. It is still beta. On 2026-11-23 the host Mac crashed during a `make build` of the Chromium image, and the disk was later found 100% full: the disk image of Apple `container`'s build VM (`~/Library/Application Support/com.apple.container/containers/buildkit`) had grown to 98 GB over three days of builds, each of which sent a vendored copy of every crate as build context. The full disk is the likely cause of the crash; it also turned Docker Desktop's storage read-only. The user chose Docker as the default and kept Apple `container` as an option.

## Considered Options

- **Keep Apple `container` as the only runtime**: rejected; its build cache grew without bound and filled the host disk, and the default Chromium image makes builds heavier.
- **Docker only, drop Apple `container`**: rejected; the user wanted to keep it as an option for when it matures.
- **Docker default, Apple `container` via `RUNTIME=container`**: chosen. Both CLIs share the verbs the `Makefile` uses, so one recipe serves both.

## Consequences

- Checks and audit evidence come from Docker; agents never run Apple `container` (REVIEW.md §5).
- `DNS` defaults to `1.1.1.1` only for `RUNTIME=container`; Docker uses the host's resolver.
- Inside a container, `citadel update` prints rebuild steps for the runtime it detects: `docker` when `/.dockerenv` exists, otherwise Apple `container` (which creates no marker file and is detected through `CITADEL_CONTAINER=1`).
- With `RUNTIME=container`, the build cache is not pruned automatically; `container builder delete` reclaims it.
- The Dockerfile itself is unchanged by this decision; it stays OCI-standard for both runtimes.
