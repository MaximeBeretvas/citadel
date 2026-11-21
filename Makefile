IMAGE ?= web-vault:latest
CONTAINER_NAME ?= web-vault
VOLUME ?= web-vault-data
PORT ?= 4317
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
	container system start
	container volume create $(VOLUME) || true
	-container stop $(CONTAINER_NAME) 2>/dev/null || true
	-container rm $(CONTAINER_NAME) 2>/dev/null || true
	container run -d --name $(CONTAINER_NAME) \
		-p 127.0.0.1:$(PORT):4317 \
		-e WEB_VAULT_HOME=/data \
		-v $(VOLUME):/data \
		$(if $(BOOKS_DIR),-v $(BOOKS_DIR):/sources/books:ro,) \
		$(IMAGE)

stop:
	container stop $(CONTAINER_NAME)

logs:
	container logs $(CONTAINER_NAME)

exec:
	container exec -i $(CONTAINER_NAME) web-vault $(CMD)

clean:
	container stop $(CONTAINER_NAME) || true
	container rm $(CONTAINER_NAME) || true
