# Example: JSON snapshot codec

Empty directory only; WAL is the same as parquet. Every checkpoint rewrites
`graph.json`, and the codec pins into `manifest.json` at the first one —
including the final checkpoint the CLI runs on a clean exit, so `.checkpoint`
is optional:

```bash
printf "CREATE (:Person {name: 'Ada'});\n.checkpoint\n" | \
  cargo run -p relate-cli -- -d ./graph-json --snapshot-format json
```

`./graph-json/graph.json` now holds the graph, e.g.:

```json
{"nodes":[{"id":0,"labels":["Person"],"properties":{"name":"Ada"}}], ...}
```

Equivalent env for empty directories (CLI, C ABI, PyO3 Python):

```bash
printf "CREATE (:Person {name: 'Ada'});\n.checkpoint\n" | \
  RELATE_SNAPSHOT_FORMAT=json cargo run -p relate-cli -- ./graph-json
```

Product shell:

```bash
cargo run -p relate-cli -- -d ./graph-json --snapshot-format json
```

Reopen without the flag. Passing `--snapshot-format parquet` on this
directory errors (`snapshot codec mismatch: directory is json, requested
parquet`, exit 1). Import GraphData: `mkdir -p ./imported && cp graph.json
./imported/graph.json` then open with `--snapshot-format json`.
