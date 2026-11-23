### Fixed

- A column alias spelled with a `__` prefix (`WITH i AS `__x` RETURN *`) is
  no longer dropped by a star projection; the column and its rows now come
  back as they do on Neo4j.
