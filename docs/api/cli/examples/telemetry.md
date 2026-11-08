# Example: inspect and control usage metrics

```bash
relate telemetry status
```

Expected stdout is the effective state plus the layer that decided it, e.g.
`enabled (default)`, `disabled (config file)`, or
`disabled (DO_NOT_TRACK)`.

```bash
relate telemetry off
```

Expected stdout is `telemetry disabled (<path to config.json>)`: the
`telemetry: false` key is persisted under
`$XDG_CONFIG_HOME/relate/config.json` (or
`~/.config/relate/config.json`, or — the Windows bases —
`%APPDATA%\relate\config.json`), any pending spooled events are
removed, and no usage metrics are sent from the
next run on. Turn it back on with:

```bash
relate telemetry on
```

Expected stdout is `telemetry enabled (<path to config.json>)`.
