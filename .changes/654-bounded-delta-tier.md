### Changed

- **Memory:** the adjacency delta tier is a bounded transient buffer — delta hop vectors reserve at a 256-hop quantum above the power-of-two band (supernode-scale updates between checkpoints hold up to 37.5% less delta heap), and settled 4–7-hop chunks seal into the compact arena (spread workloads: -64% settled delta bytes). Write latency and read paths unchanged.
