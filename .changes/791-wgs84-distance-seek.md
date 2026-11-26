### Fixed

- A POINT-index distance seek at or beyond a quarter circumference (~10,018 km)
  no longer drops points in the far hemisphere: the WGS-84 envelope took its
  latitude band from the angular radius clamped at a hemisphere, so a seek from
  0°,-10° silently omitted an 89°N point 11,021 km away, while the same
  statement answered correctly before `CREATE POINT INDEX` and with a spelling
  that defeats the index. Such radii now read the full posting set and the
  residual predicate filters it.
