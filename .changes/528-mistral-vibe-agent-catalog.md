### Added

- Mistral Vibe joins the `relate skill install` agent catalog. A machine
  with `~/.vibe` gets a `vibe:` row in `relate skill list`, and
  `relate skill install --agent vibe` (or a bare `relate skill install`,
  which targets every detected agent) writes the skill bundle to
  `~/.vibe/skills/relate` — detection, install, `skill check` drift
  detection and the `relate upgrade` refresh treat it like every other
  agent.
