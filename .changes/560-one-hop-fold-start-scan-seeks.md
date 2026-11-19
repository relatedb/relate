### Changed

- Aggregations grouped by a relationship's property with a selective
  predicate on the start node — for example `MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention)
  WHERE e.id IN $seeds RETURN e.id AS ent, r.method AS method, count(m) AS n` — no
  longer scan every node of the start label and walk every one of its
  edges: the predicate's seeks now answer the fold's start scan, walking
  only the sought nodes' adjacencies. Results are unchanged — every
  predicate still applies on the fully bound row.
