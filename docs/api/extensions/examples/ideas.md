# Example: the `ideas` package

Install the first-party package from the repository, load the seed set,
and grow the store. The positional is the idea; `--command` picks the verb.

```bash
cargo build -p relate-ideas
cp target/debug/ideas extensions/relate-ideas/package/
relate extension install ideas --from extensions/relate-ideas/package

relate run ideas -d ideas.rl --command seed
relate run ideas -d ideas.rl --command list
relate run ideas -d ideas.rl --command add "Pocket Atlas" --tag maps --tag demo
relate run ideas -d ideas.rl --command link "Pocket Atlas" --to "OpenStreetMap Wedge" --as inspired-by
relate run ideas -d ideas.rl --command related "Pocket Atlas"
relate run ideas -d ideas.rl --command show "OpenStreetMap Wedge"
relate run ideas -d ideas.rl --command list --tag maps
```

`related "Pocket Atlas"` prints:

```text
'Pocket Atlas' links:
  -> INSPIRED_BY OpenStreetMap Wedge
```

Re-running `seed` refreshes the seed ideas and never duplicates a node or
edge. Runnable form: `cargo test -p relate-ideas` drives the same verbs
through a recording host and checks the seed asset and the manifest.
