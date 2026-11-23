# Container runtime: docker (default) or container (Apple's `container` CLI, still beta).
# Both share the build/run/stop/logs/exec/rm/volume verbs used below.
RUNTIME ?= docker
IMAGE ?= citadel:latest
CONTAINER_NAME ?= citadel
VOLUME ?= citadel-data
PORT ?= 4317
# Apple `container` needs a public resolver inside its VM; Docker uses the host's DNS.
DNS ?= $(if $(filter container,$(RUNTIME)),1.1.1.1,)
BOOKS_DIR ?=
# Cargo features for the container image. "chrome" (default) enables JS rendering for
# render_js Sources; use `make build-lite` (FEATURES=) for an image without a browser.
FEATURES ?= chrome
CMD ?= list

.PHONY: install install-lite build build-lite run stop logs exec clean

# Native install into ~/.cargo/bin with the default features (includes chrome).
install:
	cargo install --path . --locked --force

# Native install without the chrome feature: no browser stack, render_js falls back to static HTTP.
install-lite:
	cargo install --path . --locked --force --no-default-features

build:
ifeq ($(RUNTIME),container)
	@mkdir -p .cargo
	@trap 'rm -rf vendor .cargo' EXIT; \
	cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml; \
	container build --build-arg FEATURES="$(FEATURES)" -t $(IMAGE) .
else
	$(RUNTIME) build --build-arg FEATURES="$(FEATURES)" -t $(IMAGE) .
endif

# Container image without chrome or chromium.
build-lite:
	$(MAKE) build FEATURES=

run:
ifeq ($(RUNTIME),container)
	container system status >/dev/null 2>&1 || container system start
endif
	$(RUNTIME) volume create $(VOLUME) >/dev/null 2>&1 || true
	-$(RUNTIME) stop $(CONTAINER_NAME) 2>/dev/null || true
	-$(RUNTIME) rm $(CONTAINER_NAME) 2>/dev/null || true
	$(RUNTIME) run --rm -u 0 --entrypoint chown -v $(VOLUME):/data $(IMAGE) -R citadel:citadel /data
	$(RUNTIME) run -d --name $(CONTAINER_NAME) \
		-p 127.0.0.1:$(PORT):4317 \
		$(if $(DNS),--dns $(DNS),) \
		-e CITADEL_HOME=/data \
		-e CITADEL_CONTAINER=1 \
		-v $(VOLUME):/data \
		$(if $(BOOKS_DIR),-v $(BOOKS_DIR):/sources/books:ro,) \
		$(IMAGE)

stop:
	$(RUNTIME) stop $(CONTAINER_NAME)

logs:
	$(RUNTIME) logs $(CONTAINER_NAME)

exec:
	$(RUNTIME) exec -i $(CONTAINER_NAME) citadel $(CMD)

clean:
	$(RUNTIME) stop $(CONTAINER_NAME) || true
	$(RUNTIME) rm $(CONTAINER_NAME) || true
