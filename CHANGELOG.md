# Changelog

All notable changes to Relate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed

- CDC replication slots. `register_cdc_slot`, `ack_cdc_slot`,
  `drop_cdc_slot`, `cdc_slots`, `cdc_records_after`, and
  `set_apply_only` / `apply_only` are gone, along with `cdc/slots.json`.
  Checkpoint recycle no longer waits on a consumer ack (ADR-0574).

## [0.1.3] — 2026-11-11

## [0.1.2] — 2026-11-09

## [0.1.1] — 2026-11-09

## [0.1.0] — 2026-11-09

### Initial release

First release of Relate — the embedded, in-memory property graph engine
with an openCypher query contract, WAL + snapshot durability, and Bolt
serving. See the README for the full surface list.

[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.3...HEAD

[0.1.3]: https://github.com/relatedb/relate/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/relatedb/relate/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0

[Unreleased]: https://github.com/relatedb/relate
