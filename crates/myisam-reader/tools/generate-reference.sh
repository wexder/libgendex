#!/usr/bin/env bash
# Optional developer tool. Normal Rust tests consume its checked-in output and need no database.
set -euo pipefail
umask 077
reference_script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
fixture_dir="${1:-$reference_script_dir/../tests/fixtures/libgen-2026-09-06}"
for reference_tool in mariadb mariadbd mariadb-install-db myisamchk node; do
  command -v "$reference_tool" >/dev/null || { echo "Missing reference tool: $reference_tool" >&2; exit 1; }
done
reference_dir="$(mktemp -d "${TMPDIR:-/tmp}/libgendex-myisam-reference.XXXXXX")"
reference_pid=""
cleanup_reference() {
  if [ -n "$reference_pid" ]; then kill "$reference_pid" 2>/dev/null || true; wait "$reference_pid" 2>/dev/null || true; fi
  rm -rf "$reference_dir"
}
trap cleanup_reference EXIT
mariadb-install-db --no-defaults --datadir="$reference_dir/db" \
  --auth-root-authentication-method=normal --skip-test-db > "$reference_dir/init.log" 2>&1
mariadbd --no-defaults --datadir="$reference_dir/db" --socket="$reference_dir/server.sock" \
  --pid-file="$reference_dir/server.pid" --skip-networking --log-error="$reference_dir/server.log" &
reference_pid=$!
for reference_attempt in {1..100}; do
  if mariadb --no-defaults --user=root --socket="$reference_dir/server.sock" -e 'SELECT 1' >/dev/null 2>&1; then break; fi
  if ! kill -0 "$reference_pid" 2>/dev/null; then cat "$reference_dir/server.log" >&2; exit 1; fi
  sleep 0.1
done
mariadb --no-defaults --user=root --socket="$reference_dir/server.sock" -e 'CREATE DATABASE fixture'
for reference_table in editions editions_add_descr editions_to_files files elem_descr; do
  cp "$fixture_dir/$reference_table.frm" "$reference_dir/db/fixture/"
  cp "$fixture_dir/$reference_table.MYD" "$reference_dir/db/fixture/"
  cp "$fixture_dir/$reference_table.MYI.header" "$reference_dir/db/fixture/$reference_table.MYI"
  # Fixture data is a bounded prefix. Rebuild only the discarded index, leaving .MYD unchanged.
  myisamchk --no-defaults --recover --quick --force --sort_buffer_size=16M --key_buffer_size=8M \
    "$reference_dir/db/fixture/$reference_table.MYI" > "$reference_dir/$reference_table.repair.log" 2>&1
  cmp "$fixture_dir/$reference_table.MYD" "$reference_dir/db/fixture/$reference_table.MYD"
done
node - "$fixture_dir" "$reference_dir" <<'JS'
const fs = require('fs');
const [fixtures, work] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(fixtures + '/manifest.json'));
for (const table of manifest.tables) {
  const fields = Object.keys(table.samples[0]);
  for (const name of [table.table, ...fields]) if (!/^[a-zA-Z0-9_]+$/.test(name)) throw Error('Invalid SQL identifier');
  const columns = fields.map(name => `'${name}', CAST(\`${name}\` AS CHAR CHARACTER SET utf8mb4)`).join(', ');
  fs.writeFileSync(work + '/' + table.table + '.sql', `SELECT JSON_OBJECT(${columns}) FROM fixture.\`${table.table}\`;\n`);
}
JS
for reference_table in editions editions_add_descr editions_to_files files elem_descr; do
  mariadb --no-defaults --user=root --socket="$reference_dir/server.sock" \
    --default-character-set=utf8mb4 --batch --raw --skip-column-names \
    < "$reference_dir/$reference_table.sql" > "$reference_dir/$reference_table.jsonl"
done
mariadb --no-defaults --user=root --socket="$reference_dir/server.sock" --batch --skip-column-names \
  -e 'SELECT VERSION()' > "$reference_dir/version.txt"
node - "$fixture_dir" "$reference_dir" <<'JS'
const fs = require('fs');
const [fixtures, work] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(fixtures + '/manifest.json'));
const tables = manifest.tables.map(table => {
  const rows = fs.readFileSync(work + '/' + table.table + '.jsonl', 'utf8').trimEnd().split('\n').map(JSON.parse);
  if (rows.length !== table.rows) throw Error(table.table + ': reference row count mismatch');
  return { table: table.table, rows };
});
fs.writeFileSync(fixtures + '/mysql-reference.json', JSON.stringify({
  engine: fs.readFileSync(work + '/version.txt', 'utf8').trim(),
  method: 'Original FRM and MYD; index rebuilt from prefix with myisamchk. MYD byte equality checked. SQL selects every fixture row.',
  tables
}, null, 2) + '\n');
console.log('Wrote independent SQL reference for ' + tables.reduce((n, t) => n + t.rows.length, 0) + ' rows.');
JS
