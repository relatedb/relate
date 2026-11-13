use super::*;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_match(&mut self, optional: bool) -> ParseResult<Clause> {
        // Cypher 25 (Neo4j 2025.06, CIP-60): the path variable declaration
        // may precede the clause-level match-mode phrase --
        // `MATCH p = REPEATABLE ELEMENTS (pattern)` -- exactly as it
        // precedes a path-mode/selector prefix (`p = TRAIL (...)`, CIP-51).
        // Recognized up front by lookahead (`name =`
        // followed by a match-mode phrase) so the phrase itself is still
        // consumed by `parse_match_mode_prefix` and the declared variable
        // lands on the first parsed path pattern.
        self.next_token(); // consume MATCH
        let mut lead_path_var: Option<SmallName> = None;
        if self.peek_named_path_prefix(0) && self.peek_match_mode_at(2).is_some() {
            let tok = self.next_token(); // consume the path variable
            self.next_token(); // consume =
            lead_path_var = Some(SmallName::from_name(&tok.literal));
        }
        let explicit_mode = self.parse_match_mode_prefix();
        let match_mode = explicit_mode.unwrap_or(MatchMode::DifferentRelationships);
        // The one path-variable production parses the leading variable with
        // the path (it wins over any inner subpath variable, the same
        // precedence `parse_pattern_path` gives its own outer-var binding).
        let pattern = match lead_path_var {
            Some(variable) => self.parse_pattern_after_leading_variable(variable)?,
            None => self.parse_pattern()?,
        };
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

    /// CIP-79: zero or more `USING INDEX <var>:<Label>(<prop>[, <prop>...])`
    /// / `USING INDEX SEEK ...` / `USING JOIN ON <var>[, <var>...]` /
    /// `USING SCAN <var>:<Label>` planner hints directly after a MATCH
    /// clause's pattern. `USING` is a non-reserved word (never added to
    /// `token.rs`'s keyword table), recognized by spelling like `SEARCH`/
    /// `FILTER` above. The SCAN variable is returned because the executor
    /// must honour it, and the INDEX hints because the front end validates
    /// their fulfillability; the JOIN hints are returned because the
    /// planner never fulfils one and the `01N30` plan-time notification
    /// reports each (see [`MatchClause::using_scan`] /
    /// [`MatchClause::using_index`] / [`MatchClause::using_join`]).
    pub(in crate::parser) fn parse_using_hints(&mut self) -> ParseResult<UsingHints> {
        let mut hints = UsingHints::default();
        while self.peek_is_ident("using") {
            self.next_token(); // consume USING
            let kind_tok = self.next_token();
            match kind_tok.literal.to_ascii_lowercase().as_str() {
                "index" => {
                    // Optional `SEEK` after `INDEX` (`USING INDEX SEEK
                    // n:Label(prop)`) -- same hint, just a stronger one.
                    if self.peek_is_ident("seek") {
                        self.next_token();
                    }
                    let variable = self.parse_bare_name("identifier")?; // variable
                    self.expect(TokenType::Colon)?;
                    let label = self.parse_label_name()?;
                    self.expect(TokenType::LeftParen)?;
                    let mut properties = Vec::new();
                    loop {
                        properties.push(self.parse_bare_name("identifier")?); // property
                        if self.peek_type() == TokenType::Comma {
                            self.next_token();
                            continue;
                        }
                        break;
                    }
                    self.expect(TokenType::RightParen)?;
                    hints.index.push(crate::ast::IndexHint {
                        variable,
                        label,
                        properties,
                    });
                }
                "join" => {
                    // `ON` is mandatory in `USING JOIN ON <var>…` — every
                    // sibling parse of the keyword in this crate expects
                    // it, and live Neo4j rejects the bare form.
                    self.expect(TokenType::ON)?;
                    let mut variables = Vec::new();
                    loop {
                        variables.push(self.parse_bare_name("identifier")?); // variable
                        if self.peek_type() == TokenType::Comma {
                            self.next_token();
                            continue;
                        }
                        break;
                    }
                    hints.join.push(crate::ast::JoinHint { variables });
                }
                "scan" => {
                    let variable = self.parse_bare_name("identifier")?;
                    self.expect(TokenType::Colon)?;
                    let label = self.parse_label_name()?;
                    hints.scan.push(crate::ast::ScanHint { variable, label });
                }
                _ => {
                    return Err(self.error_at_token(
                        &kind_tok,
                        format!(
                            "expected INDEX, JOIN, or SCAN after USING, got {:?}",
                            kind_tok.literal
                        ),
                    ))
                }
            }
        }
        Ok(hints)
    }

    /// The CIP-51/CIP-60 match mode whose two-word phrase
    /// (`REPEATABLE ELEMENTS` / `DIFFERENT RELATIONSHIPS`) starts at the
    /// token `offset` ahead, or `None` when no match-mode phrase starts
    /// there. The single spelling table for both positions a match mode
    /// can be written: directly after `MATCH`
    /// (`parse_match_mode_prefix`) and after a path variable declaration
    /// (`p = REPEATABLE ELEMENTS (...)`, looked ahead in `parse_match`).
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
