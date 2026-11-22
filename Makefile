IMAGE ?= citadel:latest
CONTAINER_NAME ?= citadel
VOLUME ?= citadel-data
PORT ?= 4317
DNS ?= 1.1.1.1
BOOKS_DIR ?=
FEATURES ?=
CMD ?= list

.PHONY: build run stop logs exec clean

build:
	@mkdir -p .cargo
	@trap 'rm -rf vendor .cargo' EXIT; \
	cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml; \
	container build $(if $(FEATURES),--build-arg FEATURES=$(FEATURES),) -t $(IMAGE) .

run:
	container system status >/dev/null 2>&1 || container system start
	container volume create $(VOLUME) || true
	-container stop $(CONTAINER_NAME) 2>/dev/null || true
	-container rm $(CONTAINER_NAME) 2>/dev/null || true
	container run --rm -u 0 --entrypoint chown -v $(VOLUME):/data $(IMAGE) -R citadel:citadel /data
	container run -d --name $(CONTAINER_NAME) \
		-p 127.0.0.1:$(PORT):4317 \
		$(if $(DNS),--dns $(DNS),) \
		-e CITADEL_HOME=/data \
		-v $(VOLUME):/data \
		$(if $(BOOKS_DIR),-v $(BOOKS_DIR):/sources/books:ro,) \
		$(IMAGE)

stop:
	container stop $(CONTAINER_NAME)

logs:
	container logs $(CONTAINER_NAME)

exec:
	container exec -i $(CONTAINER_NAME) citadel $(CMD)

clean:
	container stop $(CONTAINER_NAME) || true
	container rm $(CONTAINER_NAME) || true
