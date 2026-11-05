.PHONY: help fmt check test validate build install pwa-check pwa-build docker-build clean linux-preflight

PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
IMAGE ?= hive-sync-api:local

help:
	@printf '%s\n' 'Hive Mind developer targets:'
	@printf '%s\n' '  make fmt          Check Rust formatting'
	@printf '%s\n' '  make check        Run cargo check for the workspace'
	@printf '%s\n' '  make test         Run workspace tests'
	@printf '%s\n' '  make validate     Run fmt, check, test, shell syntax checks, and PWA check'
	@printf '%s\n' '  make build        Build release hive binary'
	@printf '%s\n' '  make install      Install hive to $$(BINDIR) (default: ~/.local/bin)'
	@printf '%s\n' '  make pwa-check    Run Svelte/PWA checks through hive'
	@printf '%s\n' '  make pwa-build    Build the PWA through hive'
	@printf '%s\n' '  make docker-build Build local hive-sync-api image (requires Docker/compatible runtime)'
	@printf '%s\n' '  make linux-preflight Check Linux development prerequisites without building or installing'

fmt:
	cargo fmt --all --check

linux-preflight:
	bash scripts/dev/linux-preflight.sh

check:
	cargo check --workspace

test:
	cargo test --workspace

validate: fmt check test
	bash -n scripts/cloud/bootstrap-hive-sync-infra.sh scripts/cloud/deploy-hive-sync-api.sh scripts/cloud/export-hive-sync-backup.sh scripts/cloud/setup-hive-sync-backups.sh scripts/cloud/setup-hive-sync-monitoring.sh
	cargo run -p hive -- pwa check

build:
	cargo build --release -p hive

install: build
	install -d "$(BINDIR)"
	install -m 0755 target/release/hive "$(BINDIR)/hive"
	@printf 'Installed hive to %s\n' "$(BINDIR)/hive"

pwa-check:
	cargo run -p hive -- pwa check

pwa-build:
	cargo run -p hive -- pwa build

docker-build:
	docker build -f Dockerfile.hive-sync-api -t "$(IMAGE)" .

clean:
	cargo clean
