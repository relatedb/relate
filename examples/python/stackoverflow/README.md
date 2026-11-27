# Stack Overflow notebook

`relate_stackoverflow.ipynb` loads the latest 100 questions (with answers) for a
Stack Overflow tag into an in-memory Relate graph and walks through: a declared
graph type, bulk `UNWIND`/`MERGE` loading, full-text and vector `SEARCH`,
exploration queries, TagOverflow-style tag co-occurrence (count and Jaccard),
and `relate.viz` / neo4j-viz rendering.

Run it:

```bash
cd surfaces/relate-python && pip install maturin && maturin develop --release
pip install "neo4j-viz>=1.12.0" jupyter
jupyter lab examples/python/stackoverflow/relate_stackoverflow.ipynb
```

Set `TAG` in the second cell to explore another tag. It needs network access to
the public Stack Exchange API, so CI does not execute it (the
`examples/python/*.ipynb` notebook job covers the offline notebooks only).

The checked-in copy carries the text outputs and the static previews in
`assets/`; the four interactive neo4j-viz outputs (about 4 MB each) are
stripped and appear when the notebook is run.
