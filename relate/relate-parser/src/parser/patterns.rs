use super::*;

/// The property spec and element `WHERE` read after a node or relationship
/// element's variable and label/type spec.
#[derive(Default)]
struct ElementTail {
    properties: Vec<(SmallName, Expr)>,
    has_properties: bool,
    properties_param: Option<Expr>,
    where_expr: Option<Expr>,
}

impl<'a> Parser<'a> {
    // ---- Pattern parsing ----

    pub(super) fn parse_pattern(&mut self) -> ParseResult<Vec<PatternPath>> {
        self.parse_pattern_after(None)
    }

    /// A pattern list whose first path carries a path variable the caller
    /// already consumed — the `MATCH p = REPEATABLE ELEMENTS …` form, where
    /// the `<name> =` prefix precedes the clause-level match-mode phrase.
    /// The variable is handed to the one path-variable production instead of
    /// being patched onto the parsed path afterwards.
    pub(super) fn parse_pattern_after_leading_variable(
        &mut self,
        variable: SmallName,
    ) -> ParseResult<Vec<PatternPath>> {
        self.parse_pattern_after(Some(variable))
    }

    fn parse_pattern_after(
        &mut self,
        mut leading_variable: Option<SmallName>,
    ) -> ParseResult<Vec<PatternPath>> {
        let mut paths = Vec::new();
        loop {
            let path = match leading_variable.take() {
                Some(variable) => self.parse_pattern_path_with_variable(variable)?,
                None => self.parse_pattern_path()?,
            };
            paths.push(path);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(paths)
    }

    /// True when the token `offset` ahead is a symbolic name directly
    /// followed by `=` -- the named-path prefix `name = ...` (ADR-0191: a
    /// keyword spelling works too). The callers pass `offset == 0` at a
    /// pattern position and `offset == 1` past a leading `(` (the
    /// parenthesized subpath group).
    pub(super) fn peek_named_path_prefix(&self, offset: usize) -> bool {
        self.peek_at(offset).ty.is_schema_name() && self.peek_at(offset + 1).ty == TokenType::Equals
    }

    pub(super) fn parse_pattern_path(&mut self) -> ParseResult<PatternPath> {
        // Optional named-path prefix: `p = ...`. Consumed up front so that a
        // path variable assigned to a top-level parenthesized path pattern
        // (`p = ((a)-[r]->(b))`, CIP-19 basic-structure #15) is recognized
        // before `parse_pattern_path_body`'s own inner double-paren handling.
        // The variable is a symbolic name, so any keyword spelling works
        // unquoted too (ADR-0191).
        let outer_var = if self.peek_named_path_prefix(0) {
            let tok = self.next_token(); // consume the path variable
            self.next_token(); // consume =
            Some(SmallName::from_name(&tok.literal))
        } else {
            None
        };
        self.parse_pattern_path_named(outer_var)
    }

    /// [`parse_pattern_path`] with the `<name> =` prefix already consumed
    /// (the path variable may precede a match-mode phrase).
    pub(super) fn parse_pattern_path_with_variable(
        &mut self,
        variable: SmallName,
    ) -> ParseResult<PatternPath> {
        self.parse_pattern_path_named(Some(variable))
    }

    fn parse_pattern_path_named(
        &mut self,
        outer_var: Option<SmallName>,
    ) -> ParseResult<PatternPath> {
        // Pre-GPM `shortestPath(...)`/`allShortestPaths(...)` used as a
        // top-level MATCH pattern (CIP-40/CIP-73 Section 3.3): sugar for
        // `SHORTEST 1 PATH (...)` / `ALL SHORTEST (...)` respectively.
        // Checked before `parse_path_prefix` -- a bare `shortestPath`
        // identifier can never otherwise start a pattern (every path
        // pattern begins with `(`), so there is no ambiguity with the
        // `ALL`/`ANY`/`SHORTEST` selector keywords.
        if self.peek_type() == TokenType::Identifier {
            let lower = self.peek().literal.to_ascii_lowercase();
            if (lower == "shortestpath" || lower == "allshortestpaths")
                && self.peek_at(1).ty == TokenType::LeftParen
            {
                self.next_token(); // consume shortestPath/allShortestPaths
                self.next_token(); // consume (
                let selector = if lower == "allshortestpaths" {
                    PathSelector::AllShortest
                } else {
                    PathSelector::Shortest {
                        count: Some(PathCount::Literal(1)),
                        group: false,
                    }
                };
                let mut path =
                    self.parse_pattern_path_body(Some(&selector), outer_var.is_some())?;
                self.expect(TokenType::RightParen)?;
                if contains_gpm_quantification(&path.elements) {
                    return Err(self.error(
                        "QuantifiedPathPatternInShortestPath: a quantified path pattern cannot be used inside shortestPath()/allShortestPaths()",
                    ));
                }
                if let Some(var) = outer_var {
                    if let Some(inner) = path.variable.take() {
                        if inner == var {
                            return Err(self.error(format!(
                                "VariableAlreadyDefined: variable `{var}` already declared"
                            )));
                        }
                        path.subpath_variable = Some(inner);
                    }
                    if path.subpath_variable.as_ref() == Some(&var) {
                        return Err(self.error(format!(
                            "VariableAlreadyDefined: variable `{var}` already declared"
                        )));
                    }
                    path.variable = Some(var);
                }
                path.mode = None;
                path.selector = Some(selector);
                path.is_shortest_path_function = true;
                return self.finish_pattern_path(path);
            }
        }

        let (selector, mode) = self.parse_path_prefix()?;

        // CIP-51 Section 3.1.2 / Deviation D1: the path variable declaration
        // must precede the mode/selector prefix. If another `name =`
        // follows the prefix we just consumed, the input was actually
        // `MODE p = ...` (mode before the variable) -- reject it explicitly
        // rather than silently reinterpreting it. A keyword spelling is a
        // path variable too (ADR-0191), so it must be rejected here the
        // same way.
        if self.peek_named_path_prefix(0) {
            return Err(self.error(
                "InvalidPathModePosition: the path variable declaration must precede the path mode/selector prefix",
            ));
        }

        let mut path = self.parse_pattern_path_body(selector.as_ref(), outer_var.is_some())?;
        if let Some(var) = outer_var {
            // A named parenthesized group that is the whole body
            // (`p = ANY SHORTEST (q = (:A)-->+(:B))`) keeps both names: the
            // element group already put `q` in `variable`, and the enclosing
            // path name `p` binds the same path value. Reusing the enclosing
            // path's own name for the group is Neo4j's 42N59 duplicate
            // declaration, not a silent collapse to one name.
            if let Some(inner) = path.variable.take() {
                if inner == var {
                    return Err(self.error(format!(
                        "VariableAlreadyDefined: variable `{var}` already declared"
                    )));
                }
                path.subpath_variable = Some(inner);
            }
            if path.subpath_variable.as_ref() == Some(&var) {
                return Err(self.error(format!(
                    "VariableAlreadyDefined: variable `{var}` already declared"
                )));
            }
            path.variable = Some(var);
        }
        path.mode = mode;
        path.selector = selector;

        self.finish_pattern_path(path)
    }

    /// Shared tail of `parse_pattern_path`: CIP-28 Section 4.2 Rule 6, a
    /// top-level path pattern must have a minimum node count > 0 -- it
    /// cannot consist entirely of zero-lower-bound quantified path patterns
    /// (which could all collapse to zero iterations, leaving no node at
    /// all). A plain `Node` element always guarantees at least one node; a
    /// `Quantified` element guarantees one only if its own quantifier
    /// requires at least one iteration.
    pub(super) fn finish_pattern_path(&self, path: PatternPath) -> ParseResult<PatternPath> {
        let has_guaranteed_node = path.elements.iter().any(|e| match e {
            PatternElement::Node(_) => true,
            PatternElement::Quantified(q) => q.quantifier.min >= 1,
            PatternElement::Relationship(_) => false,
        });
        if !has_guaranteed_node {
            return Err(self.error(
                "MinimumNodeCountZero: a path pattern must have a minimum node count greater than zero",
            ));
        }

        Ok(path)
    }

    /// Parses one path pattern's body: an optional named-path prefix
    /// (`ident = `, recognized both at the top level and for a subpath
    /// variable inside a quantification, e.g. `(p2 = (x)-->(y))*`) followed
    /// by a sequence of node, relationship, and quantified-path-pattern
    /// factors (CIP-28 Section 3.1.7 concatenation). Node and quantified
    /// factors may be juxtaposed directly (no relationship between them,
    /// e.g. `(x)((a)-->(b))*` or `((a)-->(b))*((c)-->(d))*`); a relationship
    /// factor must always be preceded by an already-parsed element and
    /// followed by a node or quantified factor.
    pub(super) fn parse_pattern_path_body(
        &mut self,
        selector: Option<&PathSelector>,
        outer_named: bool,
    ) -> ParseResult<PatternPath> {
        let mut path = PatternPath {
            variable: None,
            subpath_variable: None,
            elements: Vec::new(),
            where_expr: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        };

        // Check for named path: symbolic name = (...) -- a keyword spelling
        // included (ADR-0191), e.g. a subpath variable inside a quantified
        // path pattern (`(MATCH = (x)-->(y))*`).
        if self.peek_named_path_prefix(0) {
            let tok = self.next_token();
            self.next_token(); // consume =
            path.variable = Some(SmallName::from_name(&tok.literal));
        }

        loop {
            match self.peek_type() {
                TokenType::LeftParen if self.peek_is_parenthesized_subpath_start() => {
                    // Parenthesized subpath group: `((...) [WHERE ...])`
                    // (CIP-14). Either quantified into a `Quantified`
                    // element (CIP-28), or -- if unquantified -- legal only
                    // as the sole factor of the whole pattern.
                    self.next_token(); // consume outer (
                                       // ADR-0257: nested parenthesized subpaths recurse here
                                       // directly, bypassing `parse_expression`, so each level
                                       // must count separately (LABS-69).
                    let inner =
                        self.with_expr_depth(|p| p.parse_pattern_path_body(selector, outer_named))?;
                    let where_expr = if self.peek_type() == TokenType::WHERE {
                        self.next_token();
                        Some(self.parse_expression()?)
                    } else {
                        None
                    };
                    self.expect(TokenType::RightParen)?;

                    if matches!(
                        self.peek_type(),
                        TokenType::Star | TokenType::Plus | TokenType::LeftBrace
                    ) {
                        self.side_effects.quantifier_saw_gpm_form = true;
                        let quantifier = self.parse_quantifier()?;
                        // Rule 3: the operand's minimum path length must be
                        // > 0 -- a single node pattern cannot be quantified.
                        let has_relationship = inner
                            .elements
                            .iter()
                            .any(|e| matches!(e, PatternElement::Relationship(_)));
                        if !has_relationship {
                            return Err(self.error(
                                "MinimumPathLengthZero: the operand of a quantified path pattern must have a minimum path length greater than zero",
                            ));
                        }
                        // Rule 4: quantified path patterns cannot be nested,
                        // directly or through concatenation.
                        if contains_quantification(&inner.elements) {
                            return Err(self.error(
                                "NestedQuantification: quantified path patterns cannot be nested",
                            ));
                        }
                        path.elements
                            .push(PatternElement::Quantified(QuantifiedPathPattern {
                                subpath_variable: inner.variable.or(inner.subpath_variable),
                                elements: inner.elements,
                                where_expr,
                                quantifier,
                            }));
                    } else {
                        // Unquantified parenthesized pattern: only legal as
                        // the entire pattern path (CIP-14 pre-existing
                        // feature) -- nothing may precede or follow it.
                        if !path.elements.is_empty()
                            || matches!(
                                self.peek_type(),
                                TokenType::LeftParen | TokenType::Dash | TokenType::ArrowLeft
                            )
                        {
                            return Err(self.error(
                                "UnquantifiedParenthesizedPattern: an unquantified parenthesized path pattern cannot be juxtaposed with other pattern elements",
                            ));
                        }
                        // Neo4j 2026 (Cypher 25) supports naming a
                        // parenthesized group only under a selective path
                        // search (ANY / SHORTEST...). Under ALL, or with no
                        // selector (the implicit ALL), `(p = ...)` as the
                        // whole path body is 42N42 "sub-path assignment is
                        // not supported". The unnamed group stays legal in
                        // every selector, and so does an outer path name
                        // (`p = ((:A)-->+(:B))`). Relate keeps the
                        // CIP-28 sub-path form when the enclosing pattern
                        // carries its own path name (`p = (q = …)`), an
                        // extension the compatibility corpus does not pin.
                        if inner.variable.is_some()
                            && !outer_named
                            && !selector_is_selective(selector)
                        {
                            return Err(self.error(
                                "UnsupportedSubPathBinding: sub-path assignment is not supported",
                            ));
                        }
                        path.elements = inner.elements;
                        path.where_expr = where_expr;
                        // A doubly-parenthesized named group files its name on the inner
                        // path's `subpath_variable`, so read both fields: taking only
                        // `inner.variable` dropped the name for `((p = (:A)-->(:B)))`.
                        if let Some(v) = inner.variable.or(inner.subpath_variable) {
                            // The group's own sub-path name stays in
                            // `subpath_variable` (not `variable`, which is
                            // the enclosing `p = …` name). Keeping the two
                            // apart lets the validator tell a group reading
                            // its *own* name inside its `WHERE`
                            // (Neo4j-legal, `ANY SHORTEST (q = (…) WHERE
                            // length(q) …)`) from an enclosing path
                            // variable read there (42I21,
                            // `p = ((…) WHERE length(p) …)`): both bind the
                            // path value, so `path_names()` is unchanged.
                            path.subpath_variable = Some(v);
                        }
                        return Ok(path);
                    }
                }
                TokenType::LeftParen => {
                    let node = self.parse_node_pattern()?;
                    path.elements.push(PatternElement::Node(node));
                }
                TokenType::Dash | TokenType::ArrowLeft => {
                    if path.elements.is_empty() {
                        return Err(self.error("a pattern must start with a node pattern"));
                    }
                    let rel = self.parse_rel_pattern()?;
                    path.elements.push(PatternElement::Relationship(rel));
                    if self.peek_type() != TokenType::LeftParen {
                        return Err(self.error("expected node pattern after relationship"));
                    }
                }
                _ => break,
            }
        }

        if path.elements.is_empty() {
            return Err(self.error("expected a pattern"));
        }

        Ok(path)
    }

    /// True if the parser is positioned at the start of a parenthesized
    /// subpath group (CIP-14 `((...))`) -- either the plain `((` form, or
    /// Consumes the current token if it is the contextual identifier `s`
    /// (see `peek_is_ident`); returns whether it did. Used for the
    /// CIP-51/CIP-73/CIP-191 contextual keywords (WALK, TRAIL, ACYCLIC,
    /// SIMPLE, ANY, SHORTEST, GROUP, GROUPS, PATH, PATHS), which stay
    /// ordinary identifiers everywhere else in the grammar (so they never
    /// shadow the `all()`/`any()`/`none()`/`single()` predicate functions or
    /// a variable of the same name).
    pub(super) fn consume_ident(&mut self, s: &str) -> bool {
        if self.peek_is_ident(s) {
            self.next_token();
            true
        } else {
            false
        }
    }

    /// Parses an optional CIP-51 path mode keyword (`WALK`/`TRAIL`/
    /// `ACYCLIC`/`SIMPLE`).
    pub(super) fn parse_path_mode_keyword(&mut self) -> Option<PathMode> {
        if self.consume_ident("walk") {
            Some(PathMode::Walk)
        } else if self.consume_ident("trail") {
            Some(PathMode::Trail)
        } else if self.consume_ident("acyclic") {
            Some(PathMode::Acyclic)
        } else if self.consume_ident("simple") {
            Some(PathMode::Simple)
        } else {
            None
        }
    }

    /// Consumes an optional CIP-51/CIP-73 `PATH`/`PATHS` noise keyword
    /// (`<path keywords>` / `<path or paths>`); has no semantic effect.
    pub(super) fn consume_path_or_paths_noise(&mut self) {
        if !self.consume_ident("path") {
            self.consume_ident("paths");
        }
    }

    /// Parses the CIP-73 `<number of paths>`/`<number of groups>`
    /// non-terminal, extended by CIP-191's `<dynamic parameter
    /// specification>`: an unsigned integer literal (validated `> 0` here,
    /// Rule 0) or a bare `$parameter` (validated at runtime, once its value
    /// is known). Returns `None` if neither is present.
    pub(super) fn parse_number_of_paths(&mut self) -> ParseResult<Option<PathCount>> {
        if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            // Checked conversion via the shared bound helper, not a silent
            // clamp (LABS-81): `ANY 4294967296 PATHS` used to wrap to
            // selector 0.
            let n = self.parse_u32_bound(&tok, "InvalidNumberOfPaths: number of paths/groups")?;
            if n == 0 {
                return Err(self.error_at_token(
                    &tok,
                    "InvalidNumberOfPaths: the number of paths/groups must be greater than 0",
                ));
            }
            return Ok(Some(PathCount::Literal(n)));
        }
        if self.peek_type() == TokenType::Parameter {
            let tok = self.next_token();
            return Ok(Some(PathCount::Parameter(tok.literal.into_owned())));
        }
        Ok(None)
    }

    /// Parses the optional CIP-51 path-mode-only prefix or CIP-73/CIP-191
    /// path-search (selector) prefix that may precede a path pattern's body
    /// (`<path pattern prefix>`, CIP-51 Section 3.1.2). Returns
    /// `(selector, mode)`; both `None` when neither is written.
    pub(super) fn parse_path_prefix(
        &mut self,
    ) -> ParseResult<(Option<PathSelector>, Option<PathMode>)> {
        if self.peek_type() == TokenType::ALL {
            self.next_token(); // consume ALL
            if self.consume_ident("shortest") {
                let mode = self.parse_path_mode_keyword();
                self.consume_path_or_paths_noise();
                return Ok((Some(PathSelector::AllShortest), mode));
            }
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            return Ok((Some(PathSelector::All), mode));
        }
        if self.peek_is_ident("any") {
            self.next_token(); // consume ANY
            if self.consume_ident("shortest") {
                let mode = self.parse_path_mode_keyword();
                self.consume_path_or_paths_noise();
                return Ok((Some(PathSelector::AnyShortest), mode));
            }
            let count = self.parse_number_of_paths()?;
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            return Ok((Some(PathSelector::Any(count)), mode));
        }
        if self.peek_is_ident("shortest") {
            self.next_token(); // consume SHORTEST
            let count = self.parse_number_of_paths()?;
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            let group = self.consume_ident("group") || self.consume_ident("groups");
            if !group && count.is_none() {
                return Err(self.error(
                    "InvalidNumberOfPaths: SHORTEST requires a number of paths unless followed by GROUP/GROUPS",
                ));
            }
            return Ok((Some(PathSelector::Shortest { count, group }), mode));
        }
        // No selector: a bare path-mode-only prefix, if any.
        let mode = self.parse_path_mode_keyword();
        if mode.is_some() {
            self.consume_path_or_paths_noise();
        }
        Ok((None, mode))
    }

    /// `(name = (` when the group itself carries a subpath variable
    /// (CIP-28 Section 3.3, e.g. `(p2 = (x)-->(y))*`). Distinguishes this
    /// from a plain node pattern, whose own label/property spec never
    /// starts with `(`. The subpath variable is a symbolic name, so any
    /// keyword spelling works unquoted too (ADR-0191).
    pub(super) fn peek_is_parenthesized_subpath_start(&self) -> bool {
        if self.peek_type() != TokenType::LeftParen {
            return false;
        }
        if self.peek_at(1).ty == TokenType::LeftParen {
            return true;
        }
        self.peek_named_path_prefix(1) && self.peek_at(3).ty == TokenType::LeftParen
    }

    /// Parses a CIP-28 quantifier: `*`, `+`, `{m,n}`, `{m,}`, `{,n}`, `{,}`,
    /// or `{n}`. The current token must be `Star`, `Plus`, or `LeftBrace`.
    /// Applies Rule 1 normalization (`{,}` -> `{0,}`, an absent lower bound
    /// defaults to 0) and Rule 2 validation (upper bound > 0 if present,
    /// lower bound <= upper bound).
    pub(super) fn parse_quantifier(&mut self) -> ParseResult<PathQuantifier> {
        match self.peek_type() {
            TokenType::Star => {
                self.next_token();
                Ok(PathQuantifier { min: 0, max: None })
            }
            TokenType::Plus => {
                self.next_token();
                Ok(PathQuantifier { min: 1, max: None })
            }
            TokenType::LeftBrace => {
                self.next_token(); // consume {
                let lower = if self.peek_type() == TokenType::Integer {
                    let tok = self.next_token();
                    Some(self.parse_u32_bound(&tok, "quantifier lower bound")?)
                } else {
                    None
                };
                let (min, max) = if self.peek_type() == TokenType::Comma {
                    self.next_token(); // consume ,
                    let upper = if self.peek_type() == TokenType::Integer {
                        let tok = self.next_token();
                        Some(self.parse_u32_bound(&tok, "quantifier upper bound")?)
                    } else {
                        None
                    };
                    (lower.unwrap_or(0), upper)
                } else {
                    // No comma: a fixed quantifier `{n}`, equivalent to `{n,n}`.
                    let n = lower
                        .ok_or_else(|| self.error("expected an integer or ',' in quantifier"))?;
                    (n, Some(n))
                };
                self.expect(TokenType::RightBrace)?;
                if let Some(upper) = max {
                    if upper == 0 {
                        return Err(self.error(
                            "ZeroUpperBound: a quantifier's upper bound must be greater than zero",
                        ));
                    }
                    if min > upper {
                        return Err(self.error(
                            "LowerBoundGreaterThanUpperBound: a quantifier's lower bound cannot exceed its upper bound",
                        ));
                    }
                }
                Ok(PathQuantifier { min, max })
            }
            _ => Err(self.error("expected a quantifier")),
        }
    }

    /// ADR-0191 Decision point 4: true when the token `offset` ahead can be
    /// read as a node or relationship pattern's element variable. Only a
    /// symbolic name (`is_schema_name()`: an `Identifier` or any keyword)
    /// can be one; the `WHERE` and `IS` spellings additionally keep their
    /// existing meanings in front of the variable read, decided by a
    /// one-token lookahead:
    ///
    /// - a leading `WHERE` is the variable only when the next token cannot
    ///   start the inline element predicate (`)`, `]`, `:`, `IS`, `$`, `-`,
    ///   `<-`, `WHERE`); otherwise the inline-predicate meaning wins, so
    ///   `(WHERE 1 = 1)` stays an anonymous element with a predicate while
    ///   `(WHERE)` and `(where WHERE true)` read `WHERE` as the variable;
    /// - a leading `IS` is the variable only when the next token cannot
    ///   start a label spec; otherwise the variable-less GPM
    ///   label-introducer meaning wins, so `(IS)`/`[IS]` read `IS` as the
    ///   variable while `(IS A&B)`/`-[IS R]->` keep `IS` as the introducer.
    ///   The label-spec test is the shared offset-aware
    ///   `peek_can_start_label_expr_at`, so this lookahead and the
    ///   expression-level label reader cannot drift apart.
    ///
    /// Also used by the pattern-comprehension/predicate lookahead
    /// (`node_pattern_precedes_relationship`), which must skip exactly the
    /// tokens this reader would consume as the variable.
    pub(super) fn peek_pattern_name_is_variable(&self, offset: usize) -> bool {
        let ty = self.peek_at(offset).ty;
        if !ty.is_schema_name() {
            return false;
        }
        match ty {
            TokenType::WHERE => matches!(
                self.peek_at(offset + 1).ty,
                TokenType::RightParen
                    | TokenType::RightBracket
                    | TokenType::Colon
                    | TokenType::IS
                    | TokenType::Parameter
                    | TokenType::Dash
                    | TokenType::ArrowLeft
                    | TokenType::WHERE
            ),
            TokenType::IS => !self.peek_can_start_label_expr_at(offset + 1),
            _ => true,
        }
    }

    /// Parses the tail shared by node and relationship elements: an optional
    /// property spec -- a `{...}` map literal, or a bare `$param` (the
    /// whole-map shorthand) -- followed by an optional element `WHERE`.
    fn parse_element_property_and_where(&mut self) -> ParseResult<ElementTail> {
        let mut tail = ElementTail::default();
        if self.peek_type() == TokenType::LeftBrace {
            tail.has_properties = true;
            let pairs = self.parse_map_literal()?;
            tail.properties = pairs.into_iter().map(|p| (p.key, p.value)).collect();
        } else if self.peek_type() == TokenType::Parameter {
            tail.has_properties = true;
            let tok = self.next_token();
            tail.properties_param = Some(Expr::Parameter(tok.literal.into_owned()));
        }
        if self.peek_type() == TokenType::WHERE {
            self.next_token();
            tail.where_expr = Some(self.parse_expression()?);
        }
        Ok(tail)
    }

    pub(super) fn parse_node_pattern(&mut self) -> ParseResult<NodePattern> {
        self.expect(TokenType::LeftParen)?;
        let mut node = NodePattern {
            variable: None,
            label_expr: None,
            properties: Vec::new(),
            has_properties: false,
            properties_param: None,
            where_expr: None,
        };

        let tok = self.peek().clone();
        // The element variable is a symbolic name (ADR-0191), so any
        // keyword spelling works unquoted. `WHERE` and `IS` are read as the
        // variable only when the one-token lookahead shows their other
        // meaning cannot apply (`peek_pattern_name_is_variable`, which folds
        // in the symbolic-name check): a leading `WHERE` otherwise starts
        // the inline element predicate and a leading `IS` otherwise starts a
        // variable-less GPM label spec (`(IS A&B)`, CIP-40 Section 2.3.3).
        if self.peek_pattern_name_is_variable(0) {
            node.variable = Some(SmallName::from_name(&tok.literal));
            self.next_token();
        }

        if matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
            node.label_expr = Some(self.parse_label_spec(false)?.expr);
        }

        // Property spec, then the element-pattern WHERE: `(var:LE {props} WHERE ...)`
        // (CIP-17).
        let tail = self.parse_element_property_and_where()?;
        node.has_properties = tail.has_properties;
        node.properties = tail.properties;
        node.properties_param = tail.properties_param;
        node.where_expr = tail.where_expr;

        self.expect(TokenType::RightParen)?;
        Ok(node)
    }

    /// Parses the legacy bracket-syntax variable-length hop bounds
    /// (`*min..max`, including a bare `*`) of a relationship pattern; the
    /// current token must be `Star`. Returns `(min, max)` -- an absent
    /// upper bound is `None`, a bare `*` is `(1, None)`, and a single
    /// number `*n` is the exact hop count `(n, Some(n))`. Sets
    /// `quantifier_saw_legacy_bracket` for the clause-level
    /// quantifier-mixing check.
    fn parse_legacy_hop_bounds(&mut self) -> ParseResult<(u32, Option<u32>)> {
        debug_assert_eq!(self.peek_type(), TokenType::Star);
        self.side_effects.quantifier_saw_legacy_bracket = true;
        self.next_token();
        let lower = if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            Some(self.parse_u32_bound(&tok, "relationship hop lower bound")?)
        } else {
            None
        };
        if self.peek_type() != TokenType::DotDot {
            // `*` (unbounded) or `*n` (exact hop count).
            return Ok(lower.map_or((1, None), |n| (n, Some(n))));
        }
        self.next_token();
        let upper = if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            Some(self.parse_u32_bound(&tok, "relationship hop upper bound")?)
        } else {
            None
        };
        Ok((lower.unwrap_or(1), upper))
    }

    pub(super) fn parse_rel_pattern(&mut self) -> ParseResult<RelPattern> {
        let mut rel = RelPattern {
            variable: None,
            type_expr: None,
            properties: Vec::new(),
            has_properties: false,
            properties_param: None,
            direction: Direction::Both,
            min_hops: None,
            max_hops: None,
            where_expr: None,
            quantified: false,
        };

        let tok = self.next_token();
        let left_arrow = tok.ty == TokenType::ArrowLeft;
        if !left_arrow && tok.ty != TokenType::Dash {
            return Err(
                self.error_at_token(&tok, format!("expected - or <-, got {:?}", tok.literal))
            );
        }

        if self.peek_type() == TokenType::LeftBracket {
            self.next_token(); // consume [

            // Variable. As with node patterns, a keyword spelling is a
            // relationship variable too (ADR-0191); a bare `IS` here is the
            // variable-less GPM label introducer (`-[IS R]->`) only when
            // the lookahead does not prove it is the variable.
            let peek = self.peek().clone();
            if self.peek_pattern_name_is_variable(0) {
                rel.variable = Some(SmallName::from_name(&peek.literal));
                self.next_token();
            }

            // Types (relationship `|` disjunction predates CIP-14, so a
            // bare top-level `|` here doesn't itself count as a GQL symbol
            // for the clause-level mixing check).
            // The legacy `:A|:B` separator is only legal for a bare type
            // list; with a variable binding, an inline property map or a
            // variable length Neo4j rejects it outright (HelpfulErrorMessages
            // "should provide sensible error message when using colon in the
            // separation of alternative relationship types in failing cases").
            // The fact comes back from the type-spec parse itself, so a
            // `HasLabel` or node-pattern parse of the same spelling cannot
            // leave it set behind this element.
            let mut legacy_pipe_colon = false;
            if matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
                let spec = self.parse_label_spec(true)?;
                rel.type_expr = Some(spec.expr);
                legacy_pipe_colon = spec.saw_legacy_pipe_colon;
            }
            // The element's variable, properties and hop bounds are read
            // after the type spec, so the verdict waits until they are all
            // known (below).

            // Variable length *min..max
            if self.peek_type() == TokenType::Star {
                let (min, max) = self.parse_legacy_hop_bounds()?;
                rel.min_hops = Some(min);
                rel.max_hops = max;
                // A negation/wildcard type expression on the legacy
                // bracket-syntax variable-length form is accepted: it is
                // the same AST the GPM postfix form `()-[:!A&!B]->*()`
                // normalizes to (CIP-28 Form 5), so rejecting the bracket
                // spelling alone is a syntax-gap, not a semantic rule
                // (Neo4j GpmSyntaxMixingAcceptance "Var-length relationship
                // with GPM relationship type expression - OK"; the CIP-40
                // quantifier-mixing [12] DEVIATION row captured the older
                // rejection and is retired with it).
            }

            // Property spec, then the element-pattern WHERE (CIP-18); the WHERE is
            // accepted on the legacy bracket-syntax variable-length form
            // (`[r*1..5 WHERE ...]`) too: that is the same AST the GPM postfix
            // form `-[r WHERE ...]->{1,5}` normalizes to, so the old
            // bracket-only rejection was a syntax-gap (Neo4j
            // GpmSyntaxMixingAcceptance "Var-length relationship with inline
            // predicate - OK"; the CIP-40 quantifier-mixing [14] DEVIATION row
            // captured the older rejection and is retired with it).
            let tail = self.parse_element_property_and_where()?;
            rel.has_properties = tail.has_properties;
            rel.properties = tail.properties;
            rel.properties_param = tail.properties_param;
            rel.where_expr = tail.where_expr;

            if legacy_pipe_colon
                && (rel.variable.is_some() || rel.has_properties || rel.min_hops.is_some())
            {
                return Err(self.error(
                    "InvalidRelationshipPattern: the semantics of using colon in the separation of alternative relationship types in conjunction with the use of variable binding, inlined property predicates, or variable length is no longer supported. Please separate the relationship types with a single bar between them instead",
                ));
            }

            // A malformed relationship bracket (a stray `..`/`-`/token where the
            // closing `]` is expected — Match4 [9] `[:LIKES..]`, [10]
            // `[:LIKES*-2]`) is `SyntaxError: InvalidRelationshipPattern`,
            // not the generic `UnexpectedSyntax`.
            self.expect_detail(TokenType::RightBracket, "InvalidRelationshipPattern")?;
        }

        // Right arrow or dash
        let right_tok = self.next_token();
        match right_tok.ty {
            TokenType::ArrowRight => {
                rel.direction = if left_arrow {
                    Direction::Both
                } else {
                    Direction::Right
                };
            }
            TokenType::Dash => {
                rel.direction = if left_arrow {
                    Direction::Left
                } else {
                    Direction::Both
                };
            }
            _ => {
                return Err(self.error_at_token(
                    &right_tok,
                    format!("expected -> or -, got {:?}", right_tok.literal),
                ));
            }
        }

        // CIP-28 Section 3.1.3 Form 5 / Rule 5: a quantifier directly
        // following the relationship pattern (`-[r]->*`, `-->+`,
        // `-[]->{1,6}`, `<-[:R]-{1,3}`, `-[:R]-{2}`) normalizes to an
        // ordinary variable-length relationship pattern with implicit
        // boundary nodes: it declares no element variables of its own to
        // expose as a group, so it reuses the existing var-length matcher
        // rather than becoming a `Quantified` element. `quantified` marks
        // this origin so the engine knows to honor an inline element-pattern
        // WHERE per traversed edge (CIP-28 semantics), unlike the legacy
        // `[r*min..max]` form which this grammar position never touches.
        if matches!(
            self.peek_type(),
            TokenType::Star | TokenType::Plus | TokenType::LeftBrace
        ) {
            if rel.is_variable_length() {
                return Err(self.error(
                    "NestedQuantification: a relationship pattern cannot be quantified twice",
                ));
            }
            let quantifier = self.parse_quantifier()?;
            rel.min_hops = Some(quantifier.min);
            rel.max_hops = quantifier.max;
            rel.quantified = true;
            self.side_effects.quantifier_saw_gpm_form = true;
        }

        Ok(rel)
    }
}

/// Whether a CIP-73 path selector is *selective* in Neo4j's sense: an
/// explicit selector other than `ALL`. `None` (the implicit selector) and
/// `ALL` are the non-selective spellings; `ANY`, `ANY SHORTEST`,
/// `ALL SHORTEST` and `SHORTEST <n>` are selective. Neo4j 2026 accepts a
/// named parenthesized sub-path only under a selective search (42N42).
pub fn selector_is_selective(selector: Option<&PathSelector>) -> bool {
    matches!(
        selector,
        Some(
            PathSelector::Any(_)
                | PathSelector::AnyShortest
                | PathSelector::AllShortest
                | PathSelector::Shortest { .. }
        )
    )
}
