.PHONY: web openapi build image sample test

openapi:
	cargo run --quiet -- openapi > openapi.json

web: openapi
	cd web && npm ci && npm run build

build: web
	cargo build --release

test:
	cargo test --workspace --all-targets

image:
	docker build -t bookjev:latest .

# Writes data/sample.sql, a small dump in the libgen.li format, for trying the app without the real dumps.
sample:
	mkdir -p data && perl scripts/gen-sample-dump.pl > data/sample.sql
