# Development scripts

`gen-sample-dump.pl` generates synthetic SQL metadata for trying the application locally:

```sh
make sample
```

It requires Perl and writes `data/sample.sql`. Configuration instructions are in the
[root README](../README.md#try-it-without-the-real-dumps).

The parser's optional SQL reference generator lives in
[`crates/myisam-reader/tools`](../crates/myisam-reader/README.md#optional-fixture-capture-and-independent-reference-generation).
