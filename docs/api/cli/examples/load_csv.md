# Example: opt in to a local CSV directory

Run from the repository root (the temporary directory is removed on exit):

```bash
csv_dir=$(mktemp -d)
trap 'rm -rf "$csv_dir"' EXIT
printf 'Ada\nLin\n' > "$csv_dir/people.csv"
cargo run -p relate-cli -- --load-csv-dir "$csv_dir" :memory: \
  "LOAD CSV FROM 'people.csv' AS row RETURN row[0] AS name"

# The default cap is 8 MiB (8,388,608 bytes); one byte beyond it is rejected.
python3 - "$csv_dir/too_large.csv" <<'PY'
import sys
with open(sys.argv[1], "wb") as source:
    source.write(b"x" * (8_388_608 + 1))
PY
if output=$(cargo run -q -p relate-cli -- --load-csv-dir "$csv_dir" :memory: \
  "LOAD CSV FROM 'too_large.csv' AS row RETURN row" 2>&1); then
  echo 'expected LOAD CSV byte-limit rejection' >&2
  exit 1
fi
case "$output" in
  *'LOAD CSV file exceeds byte limit'*) printf '%s\n' "$output" ;;
  *) printf '%s\n' "$output" >&2; exit 1 ;;
esac
```

Without `--load-csv-dir` or `RELATE_LOAD_CSV_DIR`, the same query fails
with `LOAD CSV file unavailable`. The [CLI integration test](../../../../surfaces/relate-cli/tests/integration/e2e_load_csv.rs)
exercises the opt-in, default denial, environment fallback, flag precedence and
the 8 MiB + 1 byte generic limit error in CI.
