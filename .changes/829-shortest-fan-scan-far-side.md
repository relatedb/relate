### Changed

- A shortest-path pattern between two label-only endpoints, such as
  `allShortestPaths((rack:Rack)-[:HOLDS|ROUTES|CONNECTS*]-(router:Router:Egress))`,
  now lists the end label's nodes and runs the shared multi-source sweep
  with the early stop (ADR-0690). On a 3,100-node, 10,200-relationship
  network graph (600 racks, 30 egress routers) the query returns the same
  58,113 paths in about 0.4 s of CPU instead of about 1.8 s.
