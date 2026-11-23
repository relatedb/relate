### Fixed

- A composed `SHOW CURRENT USER` statement (for example `SHOW CURRENT USER YIELD user MATCH (n) RETURN n`) is now rejected off the system database with the same administration-command error as `SHOW DATABASES`; the standalone command still runs anywhere.
