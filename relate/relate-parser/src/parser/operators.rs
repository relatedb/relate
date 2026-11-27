//! The non-comparison operator spellings (ADR-0505 §2): the `&'static
//! str` constants the AST's `op` fields draw from. The comparison
//! family's set and spellings — the equality spelling included — live in
//! the token layer ([`TokenType::comparison_spelling`]), consulted
//! directly at its call sites. A submodule of the parser hub so the hub
//! keeps its clause-and-pattern concerns and every operator change lands
//! here.

/// The AST's one spelling table for non-comparison operator text
/// (ADR-0505 §2): the `op` fields of `BinaryExpr`, `UnaryExpr` and
/// `StringMatchExpr` are `&'static str` drawn from these constants, so
/// every producer consults the table and no spelling is re-spelled at a
/// build site.
pub(super) const OR: &str = "OR";
pub(super) const XOR: &str = "XOR";
pub(super) const AND: &str = "AND";
pub(super) const NOT: &str = "NOT";
/// `+`, shared by the unary and binary producers.
pub(super) const PLUS: &str = "+";
/// `-`, shared by the unary and binary producers.
pub(super) const MINUS: &str = "-";
pub(super) const CONCAT: &str = "||";
pub(super) const MUL: &str = "*";
pub(super) const DIV: &str = "/";
pub(super) const MOD: &str = "%";
pub(super) const POW: &str = "^";
pub(super) const STARTS_WITH: &str = "STARTS WITH";
pub(super) const ENDS_WITH: &str = "ENDS WITH";
pub(super) const CONTAINS: &str = "CONTAINS";
