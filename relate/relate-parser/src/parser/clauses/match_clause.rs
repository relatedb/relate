use super::*;

/// The USING-hint kind words, one table: recognition of the contextual
/// word after `USING`, the dispatch, and the fallthrough error's
/// expected-list all read [`UsingHintKind::ALL`] — a new kind forces the
/// enum, the table, and (through the exhaustive `match`) an arm, so the
/// three cannot drift apart. The index family is one variant carrying
/// its AST kind ([`IndexOf`]): a new index spelling is an
/// [`IndexHintKind`] variant plus one `ALL` row, never a copied arm, and
/// the spelling a row renders comes from [`Self::as_written`] — for the
/// index family [`IndexHintKind::as_written`], the one home of that
/// fact — so the table stores no text column to drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsingHintKind {
    /// The index-hint family: which [`IndexHintKind`] the introducer
    /// word names — the plain `INDEX`, `RANGE INDEX` and `TEXT INDEX`
    /// spellings are rows of this one variant, parsed by one dispatch
    /// arm.
    IndexOf(IndexHintKind),
    Join,
    Scan,
}

impl UsingHintKind {
    /// (kind, introducer word) — the one table; the rendered spelling is
    /// derived ([`Self::as_written`]), never stored beside it.
    const ALL: [(Self, &'static str); 5] = [
        (Self::IndexOf(IndexHintKind::Index), "index"),
        (Self::IndexOf(IndexHintKind::RangeIndex), "range"),
        (Self::IndexOf(IndexHintKind::TextIndex), "text"),
        (Self::Join, "join"),
        (Self::Scan, "scan"),
    ];

    /// The kind a contextual introducer word names, or `None`. The table
    /// owns the case-insensitive comparison (`eq_ignore_ascii_case` —
    /// the same recognition `peek_is_ident` applies to contextual
    /// words), so no caller allocates a lowered copy and case cannot
    /// silently break recognition.
    fn from_introducer(word: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, introducer)| introducer.eq_ignore_ascii_case(word))
            .map(|(kind, _)| *kind)
    }

    /// The kind as the fallthrough error's expected-list spells it —
    /// the index family's spellings come from
    /// [`IndexHintKind::as_written`] (the one home of the fact: the
    /// 42N76/01N31 renderings and this list share it); `JOIN` and
    /// `SCAN` are their own.
    fn as_written(&self) -> &'static str {
        match self {
            Self::IndexOf(kind) => kind.as_written(),
            Self::Join => "JOIN",
            Self::Scan => "SCAN",
        }
    }

    /// The kinds as the fallthrough error lists them —
    /// `INDEX, RANGE INDEX, TEXT INDEX, JOIN, or SCAN` — rendered from
    /// [`Self::ALL`] through [`Self::as_written`].
    fn rendered_list() -> String {
        let rendered: Vec<&str> = Self::ALL
            .iter()
            .map(|(kind, _)| kind.as_written())
            .collect();
        // ALL is a fixed non-empty table; an empty kind list is a state
        // the type cannot produce, so it is a loud invariant failure —
        // never a fabricated empty expected-list.
        let (last, init) = rendered
            .split_last()
            .expect("UsingHintKind::ALL is never empty");
        format!("{}, or {last}", init.join(", "))
    }
}

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_match(&mut self, optional: bool) -> ParseResult<Clause> {
        self.next_token(); // consume MATCH
                           // The match-mode phrase is legal only in this position — directly
                           // after MATCH; `parse_pattern_path_named` rejects it at every other
                           // pattern start (the rationale lives there, at the enforcing site).
        let explicit_mode = self.parse_match_mode_prefix();
        let match_mode = explicit_mode.unwrap_or(MatchMode::DifferentRelationships);
        let pattern = self.parse_pattern()?;
        self.validate_path_modes(&pattern, match_mode, explicit_mode.is_some())?;

        // Query-planner hints (CIP-79) -- `USING INDEX`/`USING INDEX SEEK`/`USING
        // SCAN`, written directly after the pattern
        // and before WHERE. Relate has no cost-based planner to steer,
        // so a JOIN hint is never fulfilled — it is kept for the `01N30`
        // plan-time notification that reports each one. `USING SCAN` is
        // kept: an indexed equality seek and a label scan answer
        // differently for a zoned-datetime DST overlap (Neo4j keys the
        // index on instant plus zone), so the hint must reach the
        // executor's candidate funnel. `USING INDEX` is kept for the front
        // end's fulfillability check (Neo4j rejects an unfulfillable hint
        // at compile time); it still steers nothing.
        let hints = self.parse_using_hints()?;

        // The pattern's WHERE clause (CIP-224) may appear either
        // before or after the `<search clause>` with no semantic
        // difference -- check for a leading WHERE first, then SEARCH, then
        // (only if no WHERE was seen yet) a trailing WHERE.
        let mut where_expr = if self.peek_type() == TokenType::WHERE {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        let search = if self.is_search_clause_start() {
            Some(self.parse_search_clause()?)
        } else {
            None
        };

        if where_expr.is_none() && self.peek_type() == TokenType::WHERE {
            self.next_token();
            where_expr = Some(self.parse_expression()?);
        }

        Ok(Clause::Match(MatchClause {
            optional,
            match_mode,
            pattern,
            using_scan: hints.scan,
            using_index: hints.index,
            using_join: hints.join,
            where_expr,
            search,
        }))
    }

    /// CIP-79: zero or more USING planner hints directly after a MATCH
    /// clause's pattern — `USING INDEX [SEEK] <var>:<Label>(<prop>[,
    /// <prop>...])`, `USING RANGE INDEX <var>:<Label>(<prop>[, ...])`
    /// and `USING TEXT INDEX <var>:<Label>(<prop>[, ...])` (neither
    /// typed spelling takes a SEEK modifier), `USING SCAN <var>:<Label>`
    /// and `USING JOIN ON <var>[, <var>...]`. `USING` is a non-reserved
    /// word (never added to `token.rs`'s keyword table), recognized by
    /// spelling like `SEARCH`/`FILTER` above. The SCAN variable is
    /// returned because the executor must honour it, and the INDEX hints
    /// (plain and typed alike) because the front end validates
    /// their fulfillability; the JOIN hints are returned because the
    /// planner never fulfils one and the `01N30` plan-time notification
    /// reports each (see [`MatchClause::using_scan`] /
    /// [`MatchClause::using_index`] / [`MatchClause::using_join`]).
    pub(in crate::parser) fn parse_using_hints(&mut self) -> ParseResult<UsingHints> {
        let mut hints = UsingHints::default();
        while self.peek_is_ident("using") {
            self.next_token(); // consume USING
            let kind_tok = self.next_token();
            match UsingHintKind::from_introducer(&kind_tok.literal) {
                Some(UsingHintKind::IndexOf(ast_kind)) => {
                    // The index family, one arm; the spellings differ
                    // only in what follows the kind word. A typed
                    // spelling (`RANGE INDEX`, `TEXT INDEX`): the INDEX
                    // word is part of the spelling, gated exactly like
                    // the join arm's `ON` (a bare `USING RANGE` or
                    // `USING TEXT` names no hint). The plain spelling:
                    // the kind word *was* the INDEX token, and the
                    // optional `SEEK` modifier may follow --
                    // `USING INDEX [SEEK] n:Label(prop)` -- same hint,
                    // just a stronger one.
                    if ast_kind != IndexHintKind::Index {
                        self.expect(TokenType::INDEX)?;
                    } else if self.peek_is_ident("seek") {
                        self.next_token();
                    }
                    hints.index.push(self.parse_index_hint_tail(ast_kind)?);
                }
                Some(UsingHintKind::Join) => {
                    // `ON` is mandatory in `USING JOIN ON <var>…` — every
                    // sibling parse of the keyword in this crate expects
                    // it, and live Neo4j rejects the bare form.
                    self.expect(TokenType::ON)?;
                    let variables = self.parse_identifier_list()?;
                    hints.join.push(crate::ast::JoinHint { variables });
                }
                Some(UsingHintKind::Scan) => {
                    let variable = self.parse_bare_name("identifier")?;
                    self.expect(TokenType::Colon)?;
                    let label = self.parse_label_name()?;
                    hints.scan.push(crate::ast::ScanHint { variable, label });
                }
                None => {
                    return Err(self.error_at_token(
                        &kind_tok,
                        format!(
                            "expected {} after USING, got {:?}",
                            UsingHintKind::rendered_list(),
                            kind_tok.literal
                        ),
                    ))
                }
            }
        }
        Ok(hints)
    }

    /// The index-hint tail after the kind words —
    /// `<variable>:<Label>(<property>[, …])` — shared by the plain,
    /// RANGE and TEXT spellings: the one parse of the variable, label
    /// and property list.
    fn parse_index_hint_tail(&mut self, kind: IndexHintKind) -> ParseResult<crate::ast::IndexHint> {
        let variable = self.parse_bare_name("identifier")?; // variable
        self.expect(TokenType::Colon)?;
        let label = self.parse_label_name()?;
        self.expect(TokenType::LeftParen)?;
        let properties = self.parse_identifier_list()?;
        self.expect(TokenType::RightParen)?;
        Ok(crate::ast::IndexHint {
            kind,
            variable,
            label,
            properties,
        })
    }

    /// The CIP-51/CIP-60 match mode whose two-word phrase
    /// (`REPEATABLE ELEMENTS` / `DIFFERENT RELATIONSHIPS`) starts at the
    /// token `offset` ahead, or `None` when no match-mode phrase starts
    /// there. The single spelling table for every reader of the phrase:
    /// `parse_match_mode_prefix` consumes it in the one legal position
    /// (directly after `MATCH`), and `parse_pattern_path_body` rejects it
    /// at a pattern-body start (`MATCH p = REPEATABLE ELEMENTS (...)`, a
    /// comma-separated later path, a `shortestPath(...)` argument), where
    /// the phrase is illegal — Neo4j accepts the mode only between `MATCH`
    /// and the graph pattern.
    /// Both keyword pairs are contextual -- plain identifiers everywhere
    /// else in the grammar -- and both are admitted in the singular too
    /// (`DIFFERENT RELATIONSHIP` per MatchModes [1.2]; `REPEATABLE
    /// ELEMENT` verified against live Neo4j 2026.10, which parses and
    /// executes it exactly like the plural).
    pub(in crate::parser) fn peek_match_mode_at(&self, offset: usize) -> Option<MatchMode> {
        // The first keyword is an identifier by construction; the second is
        // matched on its literal alone (it may lex as a keyword token).
        let second = &self.peek_at(offset + 1).literal;
        if self.peek_at_is_ident(offset, "repeatable")
            && (second.eq_ignore_ascii_case("element") || second.eq_ignore_ascii_case("elements"))
        {
            Some(MatchMode::RepeatableElements)
        } else if self.peek_at_is_ident(offset, "different")
            && (second.eq_ignore_ascii_case("relationship")
                || second.eq_ignore_ascii_case("relationships"))
        {
            Some(MatchMode::DifferentRelationships)
        } else {
            None
        }
    }

    /// Parses the optional CIP-51/CIP-60 match-mode prefix
    /// (`REPEATABLE ELEMENTS` / `DIFFERENT RELATIONSHIPS`) that may appear
    /// directly after `MATCH`. Both keyword pairs are contextual (plain
    /// identifiers everywhere else); since a graph pattern always begins
    /// with `(`, a bare identifier immediately after `MATCH` can only be one
    /// of these two phrases, so no lookahead ambiguity is possible.
    /// Returns `Some(mode)` only when a `REPEATABLE ELEMENTS`/`DIFFERENT
    /// RELATIONSHIPS` keyword was actually written, distinguishing it from
    /// the identical-valued implicit default (needed for the
    /// explicit-match-mode-vs-shortestPath-function mixing check).
    pub(in crate::parser) fn parse_match_mode_prefix(&mut self) -> Option<MatchMode> {
        let mode = self.peek_match_mode_at(0)?;
        self.next_token();
        self.next_token();
        Some(mode)
    }

    /// The path-shape rules (CIP-51), checked once a whole graph pattern
    /// (one MATCH clause's comma-separated path pattern list) has been
    /// parsed. Each `EXISTS { MATCH ... }` subquery gets its own independent
    /// call (a different "depth", rule 3), since it routes back through
    /// `parse_match`.
    /// The one MATCH-path-shape battery (CIP-40/CIP-51/CIP-60/CIP-73): shared
    /// path-mode resolution, the REPEATABLE ELEMENTS combinations, and the
    /// two clause-level mixing rules a single MATCH can break.
    pub(in crate::parser) fn validate_path_modes(
        &self,
        pattern: &[PatternPath],
        match_mode: MatchMode,
        match_mode_explicit: bool,
    ) -> ParseResult<()> {
        // CIP-40: an explicit match-mode keyword
        // cannot combine with a `shortestPath()`/`allShortestPaths()`
        // legacy function pattern in the same MATCH clause (path-selector-
        // mixing.feature scenarios 8/9) -- but combines freely with an
        // *explicit* selector keyword pattern (scenario 11) or with a
        // shortestPath function in a *separate* MATCH clause (scenario 18).
        if match_mode_explicit && pattern.iter().any(|p| p.is_shortest_path_function) {
            return Err(self.error(
                "MixingShortestPathAndMatchMode: an explicit match mode cannot be combined with a shortestPath()/allShortestPaths() pattern",
            ));
        }
        // CIP-73, path-selector-mixing.feature scenario 14: a
        // MATCH clause with more than one path pattern cannot have a
        // *selective* selector (anything but the implicit/explicit `ALL`)
        // written as an explicit keyword on any of them -- but the legacy
        // `shortestPath()`/`allShortestPaths()` function forms are exempt
        // (scenario 13: two `shortestPath()` patterns combine freely, each
        // evaluated independently). CIP-60 5.4.4/05-selective-path-patterns
        // [2]/[4]: `REPEATABLE ELEMENTS` lifts this sole-path-pattern
        // restriction. The finer-grained CIP-60 5.4.4 rules for such a MATCH
        // (selective patterns may only share *boundary* variables, and their
        // own predicates can't reference a sibling pattern's variable) are
        // not currently enforced: no TCK scenario exercises them (the
        // CIP-60 selective-path scenarios are the spec harness `cip-eval`
        // suite's).
        if match_mode != MatchMode::RepeatableElements
            && pattern.len() > 1
            && pattern
                .iter()
                .any(|p| !p.is_shortest_path_function && selector_is_selective(p.selector.as_ref()))
        {
            return Err(self.error(
                "MultiplePathPatternsWithSelectiveSelector: a MATCH clause with more than one path pattern cannot have a selective path selector",
            ));
        }

        // Rule 3: every path pattern at this depth shares one resolved path
        // mode (explicit keyword, or the Rule 1 default of WALK).
        let mut resolved: Option<PathMode> = None;
        for path in pattern {
            let m = path.mode.unwrap_or(PathMode::Walk);
            match resolved {
                None => resolved = Some(m),
                Some(prev) if prev != m => {
                    return Err(self.error(
                        "UnsupportedCombinationOfPathModes: a graph pattern cannot mix path modes",
                    ));
                }
                _ => {}
            }
        }

        if match_mode == MatchMode::RepeatableElements {
            // Rule 4: REPEATABLE ELEMENTS only combines with WALK (including
            // the implicit default).
            if let Some(m) = resolved {
                if m != PathMode::Walk {
                    return Err(self.error(
                        "UnsupportedCombinationOfMatchModeAndPathMode: REPEATABLE ELEMENTS only combines with WALK",
                    ));
                }
            }
            // Section 2.3.3.3 / Deviation D2: a top-level match rejects an
            // unbounded quantified path pattern under REPEATABLE ELEMENTS
            // (42N53 "unsafe usage of repeatable elements"), because the
            // pattern may yield infinitely many rows. It is rejected
            // unconditionally, regardless of any path selector that might
            // otherwise bound it (CIP-73: confirmed for ALL,
            // ANY, and SHORTEST alike). The doc's own claimed SHORTEST
            // relaxation (CIP-51) is intentionally *not*
            // implemented: without real relationship-uniqueness pruning
            // (which REPEATABLE ELEMENTS disables), evaluating it would mean
            // brute-force enumerating every candidate up to the group's
            // explicit hop bound before the SHORTEST selector gets a chance
            // to narrow anything down -- exponential in that bound on a
            // cyclic graph. The bound itself is admission-limited by the
            // front end's traversal-depth check (`--max-traversal-depth`,
            // LABS-610), not by any engine iteration cap: the executor has
            // none here.
            //
            // Inside an `EXISTS { … }` body the probe stops at the first
            // row, so that shape is legal there (PathModeHourglassGraph
            // "Outer WALK scope separate from inner WALK scope").
            if self.exists_subquery_depth == 0 {
                for path in pattern {
                    let unbounded = path.elements.iter().any(|e| match e {
                        PatternElement::Quantified(qpp) => qpp.quantifier.max.is_none(),
                        PatternElement::Relationship(rp) => {
                            rp.is_variable_length() && rp.max_hops.is_none()
                        }
                        PatternElement::Node(_) => false,
                    });
                    if unbounded {
                        return Err(self.error(
                            "UnsafeRepeatableElements: REPEATABLE ELEMENTS cannot be combined with an unbounded quantified path pattern",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
