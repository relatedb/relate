### Changed

- The HTTP Query API streams an auto-commit statement's rows as the engine
  produces them (ADR-0682): an answer past the stream threshold is encoded
  batch by batch into the chunked body, so a multi-million-row result no
  longer has to fit the server's memory. An error after the chunked head is
  written in-band as an `errors` member after the rows, and a client that
  stops reading ends the statement at the socket write timeout
  (`HttpConfig::write_timeout`).
