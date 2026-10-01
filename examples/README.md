# Fixture capture

`capture-myisam-fixtures.rs` is an optional developer tool for refreshing the real MyISAM
fixtures through the production FTP cache and Rust RAR decoder. It writes raw prefixes,
schema/index headers, and a manifest into the selected output directory.

Commands and reference-generation instructions are in the
[`myisam-reader` test harness documentation](../crates/myisam-reader/README.md#optional-fixture-capture-and-independent-reference-generation).
Review refreshed fixture data and regenerate the independent SQL expectations together.

For end-to-end import verification, use the binary's
[`verify-ingest` command](../docs/ingest.md#verify-the-production-ingest-path).
