### Fixed

- A list of points stored as a property now requires one CRS: a list
  mixing SRIDs — geographic beside cartesian, or 2D beside 3D within one
  family — fails with live Neo4j's `TypeError` (`Collections containing
  point values with different CRS can not be stored in properties.`)
  instead of silently storing. A mixed-CRS list is still a valid
  statement result, and a single point property is unaffected.
