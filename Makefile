# Moira — local development entry points.
#
# **Targets GNU Make 3.81**, which is what macOS ships. That rules out `.ONESHELL`,
# `.SHELLFLAGS` and `$(file ...)`: every recipe line below is its own shell, and
# anything needing a heredoc lives in `scripts/` instead.
#
# **The one thing this file exists to get right.** `Settings::load` reads
# `config/default.toml`, then `config/local.toml`, then the process environment.
# It does NOT read `.env` — no dotenv crate is in the dependency tree. So a bare
# `cargo run` in this repo does not fail; it starts on `config/default.toml`
# defaults, which means no database, `allow_insecure_dev_key = true` and
# `allow_insecure_dev_pepper = true`. That process looks healthy and is not the
# one you configured. Every target here sources `.env` first.
#
#   make            list the targets
#   make setup      one-time: generate .env, start Postgres/Redis, migrate, mint a key
#   make start      Postgres/Redis up, migrations applied, API in the foreground

SHELL := /bin/bash
.DEFAULT_GOAL := help

# Sourcing prefix. `set -a` exports everything the file defines for the command
# that follows it. Guarded so a missing `.env` is a no-op rather than a failed
# recipe line, and terminated with `set +a` so the line's exit status is that of
# the real command.
ENV := set -a; [ -f .env ] || : ; [ ! -f .env ] || . ./.env; set +a;

# Read back from `.env` so `make health` follows a changed port instead of
# probing 8080 and reporting the service down.
PORT := $(shell [ ! -f .env ] || . ./.env; echo $${MOIRA_SERVER__PORT:-8080})
HOST := $(shell [ ! -f .env ] || . ./.env; echo $${MOIRA_SERVER__HOST:-127.0.0.1})
BASE := http://$(HOST):$(PORT)

CONSOLE_PORT ?= 3000
# The console's OWN database. Never Moira's: they keep independent migration
# ledgers, and one `search_path` holding both is the failure that separation exists
# to prevent.
CONSOLE_DB_URL ?= postgres://postgres:postgres@127.0.0.1:5432/moira_console

# `make execute-test PROMPT="..." ROUTE=...`
PROMPT ?= Hello
ROUTE ?= general

.PHONY: help setup start env env-force env-rotate up down reset logs ps psql redis \
        build migrate run serve release bootstrap-key seed test-seed-local smoke execute-test \
        keyring rotate-keys rotation-gate health openapi docs \
        console-install console-db console-dev console-build console-start console-check \
        fmt fmt-check clippy test gates gates-fast check doctor clean

##@ Getting started

help: ## List these targets
	@printf '\033[1mmoira\033[0m — local development\n'
	@# One physical line on purpose: a backslash-continued awk program inside single
	@# quotes reaches awk with the continuation embedded rather than resolved, and the
	@# first pattern then silently stops matching.
	@awk 'BEGIN {FS = ":.*##"} /^##@/ { printf "\033[1m%s\033[0m\n", substr($$0, 5) } /^[a-zA-Z0-9_-]+:.*##/ { printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@printf '\nAPI: %s   console: http://localhost:%s\n\n' "$(BASE)" "$(CONSOLE_PORT)"

setup: env console-install up migrate bootstrap-key ## One-time: env files, deps, containers, schema, system key
	@printf '\nSetup complete. `make start` to run the API.\n'
	@printf 'Then, in a second shell: `make seed` (a provider to route to) and `make smoke`.\n'

start: up migrate run ## Containers up, migrations applied, API in the foreground

##@ Environment

env: ## Generate .env and console/.env.local with fresh keys (keeps existing files)
	@scripts/dev-env.sh

env-force: ## Rewrite both env files, keeping the existing keys and port (.bak backups)
	@scripts/dev-env.sh --force

env-rotate: ## DESTRUCTIVE — rewrite AND mint new keys; every sealed row becomes unreadable
	@# The master key seals provider_credentials, the API-key pepper hashes every
	@# live key, and BETTER_AUTH_SECRET encrypts the console's stored signing key.
	@# Nothing errors on rotation; the old rows just stop being readable at use time.
	@printf 'Mints a new master key and both peppers. Every stored credential and API\n'
	@printf 'key becomes unusable, with no error until something tries to read one.\n'
	@printf 'Type yes: ' && read -r a && [ "$$a" = yes ]
	@scripts/dev-env.sh --force --rotate-keys

doctor: ## Report toolchain, containers and listening ports
	@printf 'cargo      %s\n' "$$(cargo --version 2>/dev/null || echo MISSING)"
	@printf 'docker     %s\n' "$$(docker --version 2>/dev/null || echo MISSING)"
	@printf 'bun        %s\n' "$$(bun --version 2>/dev/null || echo 'MISSING — needed only for console/')"
	@printf 'node       %s (console/.nvmrc wants %s)\n' "$$(node --version 2>/dev/null || echo MISSING)" "$$(cat console/.nvmrc)"
	@printf '.env       %s\n' "$$([ -f .env ] && echo present || echo 'MISSING — run `make env`')"
	@printf 'console    %s\n' "$$([ -f console/.env.local ] && echo '.env.local present' || echo '.env.local MISSING — run `make env`')"
	@printf 'node_mods  %s\n' "$$([ -d console/node_modules ] && echo present || echo 'MISSING — run `make console-install`')"
	@printf '\ncontainers\n'
	@docker compose ps --format '  {{.Service}}\t{{.Status}}' 2>/dev/null || printf '  (docker not running)\n'
	@printf '\nlistening\n'
	@lsof -nP -iTCP:$(PORT) -iTCP:5432 -iTCP:6379 -iTCP:$(CONSOLE_PORT) -sTCP:LISTEN 2>/dev/null \
		| awk 'NR>1 {printf "  %-10s %s\n", $$1, $$9}' | sort -u || printf '  (none)\n'

##@ Infrastructure

up: ## Start Postgres (pgvector) and Redis, waiting for both to report healthy
	docker compose up -d --wait

down: ## Stop the containers, keeping the volumes
	docker compose down

reset: ## DESTRUCTIVE — stop the containers and delete both data volumes
	@printf 'Deletes the moira-postgres and moira-redis volumes. All local data goes. Type yes: ' \
		&& read -r a && [ "$$a" = yes ]
	docker compose down -v

logs: ## Follow the container logs
	docker compose logs -f

ps: ## Show container status
	docker compose ps

psql: ## Open psql against the Moira database
	docker compose exec postgres psql -U postgres -d moira

redis: ## Open redis-cli
	docker compose exec redis redis-cli

##@ API

build: ## Compile the service
	$(ENV) cargo build

migrate: ## Apply the SQL migrations as an explicit step
	$(ENV) cargo run --quiet -- migrate

run: ## Run the API in the foreground (Ctrl-C to stop)
	$(ENV) cargo run

serve: run ## Alias for `run`

release: ## Build the optimised binary
	$(ENV) cargo build --release --locked

bootstrap-key: ## Mint a root system key into .env and console/.env.local (never printed)
	$(ENV) scripts/bootstrap-key.sh

seed: ## Register a provider/model/credential/policy so a prompt has somewhere to go
	@# A migrated database is not a runnable one: 0005 seeds the `general` route and
	@# nothing else, so `POST /api/v1/responses` answers 404 credential_not_found
	@# until this has run. Override the target with MOIRA_SEED_BASE_URL / _MODEL.
	$(ENV) scripts/seed-local.sh

test-seed-local: ## Unit-test scripts/seed-local.sh's pure logic — no server, no database
	python3 -m unittest discover -s scripts -p 'seed_local_lib_test.py' -v

smoke: ## End-to-end check: health, contract, and a real completion with real tokens
	$(ENV) scripts/smoke.sh

execute-test: ## Exercise the execution kernel: make execute-test PROMPT="..." ROUTE=general
	$(ENV) cargo run --quiet -- execute-test -- --prompt "$(PROMPT)" --route "$(ROUTE)"

keyring: ## Inspect or rotate the content keyring: make keyring ARGS="status"
	$(ENV) cargo run --quiet -- keyring $(ARGS)

rotate-keys: build ## Perform a REAL R1 and R2 against the local database (see scripts/rotate-keys.sh)
	@# Not a smoke test with a different name. Rotation code is usually broken the first
	@# time it is needed, because it runs once every few years under pressure on a path
	@# nothing exercises; the point of a `make` target is that the path gets exercised
	@# routinely and by humans. `cargo test` proves the functions work — this proves the
	@# BINARY does: argv, the process mode, settings, custody, and the operator's output.
	@#
	@# Neither verb touches a `*_encrypted` column, and the run leaves your keyring
	@# wrapped under the master key `.env` already configures, so nothing needs editing
	@# afterwards.
	$(ENV) scripts/rotate-keys.sh

health: ## Probe /health/live and /health/ready
	@curl -fsS $(BASE)/health/live >/dev/null && printf 'live   ok\n' || { printf 'live   FAILED — nothing answering on %s\n' "$(BASE)"; exit 1; }
	@curl -fsS $(BASE)/health/ready >/dev/null && printf 'ready  ok\n' || { printf 'ready  FAILED — process is up but a dependency is not\n'; exit 1; }

openapi: ## Fetch the OpenAPI 3.1 document
	@curl -fsS $(BASE)/openapi.json

docs: ## Open the Scalar API reference
	@open $(BASE)/docs 2>/dev/null || printf 'Open %s/docs\n' "$(BASE)"

##@ Console (Next.js BFF)

console-install: ## Install the console's dependencies
	cd console && bun install

console-db: ## Create and migrate the console's own database, then enable it in .env.local
	docker compose exec -T postgres psql -U postgres -tc \
		"SELECT 1 FROM pg_database WHERE datname='moira_console'" | grep -q 1 \
		|| docker compose exec -T postgres createdb -U postgres moira_console
	cd console && bun run db:migrate --url "$(CONSOLE_DB_URL)"
	@# `.sedtmp`, not `.bak`: BSD sed requires a suffix, and cleaning up a `.bak` here
	@# would delete the backup `make env-force` leaves behind under that same name.
	@sed -i.sedtmp 's|^# CONSOLE_DATABASE_URL=|CONSOLE_DATABASE_URL=|' console/.env.local && rm -f console/.env.local.sedtmp
	@printf 'CONSOLE_DATABASE_URL enabled in console/.env.local — restart `make console-dev`.\n'

console-dev: ## Run the console dev server (CONSOLE_PORT, default 3000)
	cd console && bun run dev --port $(CONSOLE_PORT)

console-build: ## Production build of the console
	cd console && bun run build

console-start: ## Serve the production console build
	cd console && bun run start --port $(CONSOLE_PORT)

console-check: ## Console lint, typecheck and unit tests
	cd console && bun run lint && bun run typecheck && bun test

##@ Quality

fmt: ## Format
	cargo fmt

fmt-check: ## Verify formatting
	cargo fmt --check

clippy: ## Lint, warnings denied
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test: ## Run the test suite against the local database
	$(ENV) cargo test --workspace --all-features

rotation-gate: ## The keyring-rotation gate — fails when the test database is absent, never skips
	$(ENV) scripts/rotation-gate.sh

gates: ## The six merge gates (scripts/gates.sh)
	$(ENV) scripts/gates.sh

gates-fast: ## Gates minus release/deny/audit — inner loop, NOT sufficient to merge
	$(ENV) scripts/gates.sh --fast

check: fmt-check clippy test ## fmt-check, clippy and test

clean: ## Remove the cargo build directory
	cargo clean
