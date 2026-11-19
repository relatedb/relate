//! The parser's clause-level side-effect flags: the label-mixing and
//! quantifier-form accumulators a pattern parse can set, kept as one
//! `Copy` field on `Parser` (`Parser::side_effects`) so a speculative
//! probe snapshots and restores them by plain assignment
//! ([`Parser::probe`](super::Parser::probe)) and the clause-start reset
//! is a single `Default::default()` (plus the two quantifier flags'
//! per-clause reset for nested clauses). Home of the accumulator types
//! and the monotonic delta vocabulary the speculation tree cache
//! replays ([`PatternSideEffectFlags::delta_from`] /
//! [`PatternSideEffectFlags::absorb`]).

/// The clause-level side-effect flags a pattern parse can set. Kept as one
/// `Copy` field on `Parser` (`Parser::side_effects`) so a speculative
/// `parse_pattern_path` probe snapshots and restores them by plain
/// assignment (see `pattern_comprehension_follows_equals`), and the
/// clause-start reset is a single `Default::default()` (plus the two
/// quantifier flags' per-clause reset for nested clauses).
#[derive(Default, Clone, Copy)]
pub(super) struct PatternSideEffectFlags {
    /// The label-mixing forms seen in the clause currently being parsed.
    /// `parse_insert` reads this for its own clause-local rejection (a
    /// `:A:B` chain is never allowed in INSERT); it is reset at the start of
    /// every clause, nested or not.
    pub(super) clause_labels: LabelMixFlags,
    /// The label-mixing forms seen by the read clauses accumulated in the
    /// current scope. Neo4j keeps the read side (MATCH/WITH/RETURN label
    /// predicates) and the write side (CREATE/INSERT; MERGE counts as both)
    /// apart: inside one `CALL { … }` body `MATCH (n:A:B)` followed by
    /// `CREATE (m IS B)` is legal, while two MATCHes (or a MERGE and a
    /// CREATE) mixing the forms are not. See `PatternSideEffectFlags::read_labels`.
    pub(super) read_labels: LabelMixFlags,
    /// The write side of the split described on [`Self::read_labels`].
    pub(super) write_labels: LabelMixFlags,
    /// The forms the ONE label/type expression currently being parsed has
    /// seen (`legacy_conjunction` and `gql_symbol` only). Reset at the
    /// entry of `parse_label_expr_body`; `mixed-label-syntax` admits
    /// mixing across separate label expressions of one clause, but Neo4j
    /// keeps it illegal *within* one expression
    /// (GpmSyntaxMixingAllowedAcceptance "Mixing : conjunction with
    /// GPM-only label/type expression operators": `(n:A|B:C)`).
    pub(super) expression_labels: LabelMixFlags,
    /// True if a parenthesized label-expression group (`(A:B)`, `(A&B)`,
    /// ...) was parsed anywhere in the clause currently being parsed.
    /// CIP-114: CREATE/MERGE/MATCH allow this grouping, but INSERT does
    /// not -- `parse_insert` checks this flag after parsing its pattern,
    /// same reset/scoping as [`Self::clause_labels`].
    pub(super) label_saw_paren_group: bool,
    /// The quantifier forms seen anywhere in the clause currently being
    /// parsed. Neo4j rejects mixing the pre-GPM bracket quantifier
    /// (`[*min..max]`, including a bare `[*]`) with a GPM postfix/QPP
    /// quantifier (`->+`, `->*`, `->{m,n}`, `((...)...){m,n}`) *within the
    /// same clause* -- but not across separate clauses/statements (CIP-40
    /// quantifier-mixing.feature scenario 9 vs scenario 10), checked in
    /// `check_quantifier_mixing`.
    pub(super) quantifiers: QuantifierForms,
    /// Quantifier forms seen inside pattern comprehensions of the clause
    /// currently being parsed (accumulated across all of its
    /// comprehensions). A comprehension's inner pattern is its own pattern
    /// context: its quantifier forms never combine with the clause's own
    /// (a legacy `[:R*1..2]` inside a comprehension nested in a quantified
    /// path pattern's WHERE is legal, Neo4j
    /// QuantifiedPathPatternAcceptance "Pattern comprehension containing a
    /// variable-length relationship inside a QPP is allowed"), but two
    /// comprehensions in the same clause still combine with each other
    /// (CIP-40 label-expression-mixing.feature scenario 13,
    /// quantifier-mixing.feature scenario 10) -- so these are a separate
    /// accumulator, checked separately from [`Self::quantifiers`].
    pub(super) comprehension_quantifiers: QuantifierForms,
}

impl PatternSideEffectFlags {
    /// The side-effect delta of a parse that ran from `before` to
    /// `after`, for replay by a later parse of the same tokens
    /// ([`Self::absorb`]). The monotone fields carry the difference
    /// (`after` minus `before`). The scope-reset fields — `quantifiers`
    /// (a comprehension body `mem::take`s it) and `expression_labels`
    /// (each label-expression body re-defaults it) — carry the parse's
    /// **absolute** end-state, so ambient context bits cannot mask the
    /// parse's own forms: probes run these two from `Default`
    /// ([`Parser::probe`]), so the absolute value is exactly the set of
    /// forms the parse itself noted. (`label_mix_target` is not one of
    /// these fields — the clause dispatcher owns it on the parser, and
    /// pattern parsing never changes it.)
    pub(super) fn delta_from(before: &Self, after: &Self) -> Self {
        let mix = |b: LabelMixFlags, a: LabelMixFlags| LabelMixFlags {
            legacy_conjunction: a.legacy_conjunction && !b.legacy_conjunction,
            gql_symbol: a.gql_symbol && !b.gql_symbol,
            is_introducer: a.is_introducer && !b.is_introducer,
        };
        let forms = |b: QuantifierForms, a: QuantifierForms| QuantifierForms {
            legacy_bracket: a.legacy_bracket && !b.legacy_bracket,
            gpm_form: a.gpm_form && !b.gpm_form,
        };
        Self {
            clause_labels: mix(before.clause_labels, after.clause_labels),
            read_labels: mix(before.read_labels, after.read_labels),
            write_labels: mix(before.write_labels, after.write_labels),
            expression_labels: after.expression_labels,
            label_saw_paren_group: after.label_saw_paren_group && !before.label_saw_paren_group,
            quantifiers: after.quantifiers,
            comprehension_quantifiers: forms(
                before.comprehension_quantifiers,
                after.comprehension_quantifiers,
            ),
        }
    }

    /// Replays a footprint into the current accumulators (the commit-side
    /// twin of [`Self::delta_from`]). The monotone fields union — the
    /// parse's forms join whatever the context already holds. The
    /// scope-reset fields replay the parse's absolute end-state:
    /// `quantifiers` unions it (a node-pattern parse only ever adds
    /// forms to the scope it runs in), and `expression_labels` is
    /// assigned (a label-expression body re-defaults the field on
    /// entry, so the parse's own end-state is what a real re-parse
    /// leaves behind — including a nested label body's reset of an
    /// outer accumulation, the same behavior a cache-less parse has).
    pub(super) fn absorb(&mut self, delta: &Self) {
        self.clause_labels.union_with(delta.clause_labels);
        self.read_labels.union_with(delta.read_labels);
        self.write_labels.union_with(delta.write_labels);
        self.expression_labels = delta.expression_labels;
        self.label_saw_paren_group |= delta.label_saw_paren_group;
        self.quantifiers.absorb(delta.quantifiers);
        self.comprehension_quantifiers
            .absorb(delta.comprehension_quantifiers);
    }
}

/// The two quantifier syntaxes whose mixing Neo4j rejects within one clause.
#[derive(Default, Clone, Copy)]
pub(super) struct QuantifierForms {
    /// A legacy bracket-syntax variable-length quantifier (`[*min..max]`).
    pub(super) legacy_bracket: bool,
    /// A GPM postfix or QPP quantifier.
    pub(super) gpm_form: bool,
}

impl QuantifierForms {
    pub(super) fn mixed(self) -> bool {
        self.legacy_bracket && self.gpm_form
    }

    /// Folds another accumulator's forms into this one.
    pub(super) fn absorb(&mut self, other: Self) {
        self.legacy_bracket |= other.legacy_bracket;
        self.gpm_form |= other.gpm_form;
    }
}

/// The three label-expression forms whose mixing Neo4j rejects within one
/// clause (CIP-14/CIP-40).
#[derive(Default, Clone, Copy)]
pub(super) struct LabelMixFlags {
    /// A legacy multi-label colon-conjunction (`:A:B`).
    pub(super) legacy_conjunction: bool,
    /// A GQL label-expression symbol (`&`, `!`, `%`, a grouping paren, or
    /// `|` on a node).
    pub(super) gql_symbol: bool,
    /// A pattern-position `IS` label/type introducer (`(n IS A)`,
    /// `-[IS R]->`), even without any GQL symbol beside it.
    pub(super) is_introducer: bool,
}

impl LabelMixFlags {
    /// Bit-union: folds another accumulator's forms into this one (the
    /// [`QuantifierForms::absorb`] spelling, for the label flags).
    pub(super) fn union_with(&mut self, other: Self) {
        self.legacy_conjunction |= other.legacy_conjunction;
        self.gql_symbol |= other.gql_symbol;
        self.is_introducer |= other.is_introducer;
    }

    /// The within-one-expression rule (used on the `expression_labels`
    /// accumulator): colon-conjunction beside a GQL symbol.
    pub(super) fn within_expression_mixing(&self) -> bool {
        self.legacy_conjunction && self.gql_symbol
    }

    /// The rejection this set of forms triggers, if any. The GQL-symbol rule
    /// is reported before the `IS` rule, matching the reference engine's
    /// 42I10-before-42I29 precedence.
    pub(super) fn mixing(&self) -> Option<&'static str> {
        if self.legacy_conjunction && self.gql_symbol {
            return Some(
                "InvalidLabelExpression: mixing label expression symbols (|, &, !, and %) with colon (:) between labels is not allowed",
            );
        }
        if self.legacy_conjunction && self.is_introducer {
            return Some(
                "MixedIsAndColonConjunction: cannot mix the IS introducer with colon-conjunction within the same clause",
            );
        }
        None
    }
}

/// Which accumulator(s) the label forms of the clause being parsed feed.
/// Neo4j keeps read clauses and write clauses apart when combining the forms
/// across the clauses of one subquery scope; MERGE is both, and a label
/// specification written by SET/REMOVE never joins either side.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LabelMixTarget {
    #[default]
    None,
    Read,
    Write,
    Both,
}
