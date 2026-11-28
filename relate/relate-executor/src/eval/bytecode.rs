//! Flat stack-based bytecode evaluator for single-node predicates (ADR-0720).
//!
//! Compiles arbitrary boolean combinations (`AND`, `OR`, `NOT`), comparisons
//! (`is_pred_op`, including mirrored comparisons), null checks (`IS [NOT] NULL`),
//! and multi-entry property maps over a single node variable into a flat
//! `Vec<Op>` program. Row-invariant bounds are evaluated lazily once per stage
//! on the first candidate that reaches the program, and a stage-owned `Vec<Value>`
//! stack is reused across candidates (zero per-candidate stack allocation).

use relate_core::{NodeId, Result, Value};
use relate_ir::{var_prop_named, BinaryOp, Expr, UnaryOp};
use relate_names::NamedToken;

use crate::ctx::ExecCtx;
use crate::eval::{binary_pure, eval, orient};
use crate::row::Row;

use crate::exec::agg_fold::is_pred_op;
use crate::exec::kernels::program::bound_is_row_invariant;

/// One instruction of a single-node predicate program.
#[derive(Clone, Copy, Debug)]
enum Op {
    /// Push `graph.node_prop_resolved(node, &keys[idx]).unwrap_or(Value::Null)`.
    LoadProp(u16),
    /// Push `prepared_consts[idx].clone()`.
    LoadConst(u16),
    /// Pop `(lhs, rhs)` and push `binary_pure(op, lhs, rhs)?`.
    Cmp(BinaryOp),
    /// Pop `v` and push `Value::Bool(v.is_null())`.
    IsNull,
    /// Pop `v` and push `Value::Bool(!v.is_null())`.
    IsNotNull,
    /// Pop `v` and push Cypher 3-valued `NOT v`.
    Not,
    /// Pop `(lhs, rhs)` and push Cypher 3-valued `lhs AND rhs`.
    And,
    /// Pop `(lhs, rhs)` and push Cypher 3-valued `lhs OR rhs`.
    Or,
}

/// A compiled single-node predicate program over one variable.
#[derive(Clone, Debug)]
pub(crate) struct NodeBytecode {
    ops: Vec<Op>,
    keys: Vec<NamedToken>,
    consts: Vec<Expr>,
}

/// Stage-prepared state for [`NodeBytecode`]: once-evaluated row-invariant
/// constants plus a reusable evaluation stack.
pub(crate) struct PreparedBytecode {
    consts: Vec<Value>,
    stack: Vec<Value>,
}

impl NodeBytecode {
    /// Compile a single expression known to reference at most `var` into a
    /// [`NodeBytecode`] program. Returns `None` if any sub-expression falls
    /// outside the single-node predicate vocabulary.
    pub(crate) fn compile_expr(var: &str, expr: &Expr) -> Option<Self> {
        let mut builder = Builder::default();
        builder.emit_bool(var, expr)?;
        Some(builder.finish())
    }

    /// Compile a list of `(key, value)` property-map equality entries plus
    /// optional additional conjuncts (all over `var`) into one [`NodeBytecode`].
    pub(crate) fn compile_entries_and_exprs<'a>(
        var: &str,
        entries: impl IntoIterator<Item = (NamedToken, &'a Expr)>,
        conjuncts: impl IntoIterator<Item = &'a Expr>,
    ) -> Option<Self> {
        let mut builder = Builder::default();
        let mut count = 0usize;
        for (key, value) in entries {
            if !bound_is_row_invariant(value) {
                return None;
            }
            let k = builder.intern_key(key)?;
            let c = builder.push_const(value.clone())?;
            builder.ops.push(Op::LoadProp(k));
            builder.ops.push(Op::LoadConst(c));
            builder.ops.push(Op::Cmp(BinaryOp::Eq));
            if count > 0 {
                builder.ops.push(Op::And);
            }
            count += 1;
        }
        for expr in conjuncts {
            builder.emit_bool(var, expr)?;
            if count > 0 {
                builder.ops.push(Op::And);
            }
            count += 1;
        }
        (count > 0).then(|| builder.finish())
    }

    /// Lazily evaluate all row-invariant constant operands once per stage.
    pub(crate) fn prepare(&self, ctx: ExecCtx<'_>) -> Result<PreparedBytecode> {
        let empty = Row::new();
        let mut consts = Vec::with_capacity(self.consts.len());
        for expr in &self.consts {
            consts.push(eval(ctx.graph, &empty, expr, ctx.params)?);
        }
        Ok(PreparedBytecode {
            consts,
            stack: Vec::with_capacity(8),
        })
    }

    /// Evaluate the compiled predicate against `node`, returning `true` iff
    /// the three-valued result is `Some(true)`.
    pub(crate) fn admits(
        &self,
        prepared: &mut PreparedBytecode,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        let stack = &mut prepared.stack;
        stack.clear();
        for &op in &self.ops {
            match op {
                Op::LoadProp(idx) => {
                    let key = &self.keys[idx as usize];
                    let val = ctx
                        .graph
                        .node_prop_resolved(node, key)
                        .unwrap_or(Value::Null);
                    stack.push(val);
                }
                Op::LoadConst(idx) => {
                    stack.push(prepared.consts[idx as usize].clone());
                }
                Op::Cmp(bin_op) => {
                    let rhs = stack.pop().expect("bytecode stack underflow (rhs)");
                    let lhs = stack.pop().expect("bytecode stack underflow (lhs)");
                    let res = binary_pure(bin_op, lhs, rhs)?;
                    stack.push(res);
                }
                Op::IsNull => {
                    let v = stack.pop().expect("bytecode stack underflow (IsNull)");
                    stack.push(Value::Bool(v.is_null()));
                }
                Op::IsNotNull => {
                    let v = stack.pop().expect("bytecode stack underflow (IsNotNull)");
                    stack.push(Value::Bool(!v.is_null()));
                }
                Op::Not => {
                    let v = stack.pop().expect("bytecode stack underflow (Not)");
                    let out = match v.truthy() {
                        Some(b) => Value::Bool(!b),
                        None => Value::Null,
                    };
                    stack.push(out);
                }
                Op::And => {
                    let rhs = stack.pop().expect("bytecode stack underflow (And rhs)");
                    let lhs = stack.pop().expect("bytecode stack underflow (And lhs)");
                    let out = match (lhs.truthy(), rhs.truthy()) {
                        (Some(false), _) | (_, Some(false)) => Value::Bool(false),
                        (Some(true), Some(true)) => Value::Bool(true),
                        _ => Value::Null,
                    };
                    stack.push(out);
                }
                Op::Or => {
                    let rhs = stack.pop().expect("bytecode stack underflow (Or rhs)");
                    let lhs = stack.pop().expect("bytecode stack underflow (Or lhs)");
                    let out = match (lhs.truthy(), rhs.truthy()) {
                        (Some(true), _) | (_, Some(true)) => Value::Bool(true),
                        (Some(false), Some(false)) => Value::Bool(false),
                        _ => Value::Null,
                    };
                    stack.push(out);
                }
            }
        }
        Ok(stack.pop().and_then(|v| v.truthy()) == Some(true))
    }
}

#[derive(Default)]
struct Builder {
    ops: Vec<Op>,
    keys: Vec<NamedToken>,
    consts: Vec<Expr>,
}

impl Builder {
    fn intern_key(&mut self, key: NamedToken) -> Option<u16> {
        if let Some(pos) = self.keys.iter().position(|(name, _)| *name == key.0) {
            return u16::try_from(pos).ok();
        }
        let idx = u16::try_from(self.keys.len()).ok()?;
        self.keys.push(key);
        Some(idx)
    }

    fn push_const(&mut self, expr: Expr) -> Option<u16> {
        let idx = u16::try_from(self.consts.len()).ok()?;
        self.consts.push(expr);
        Some(idx)
    }

    fn emit_bool(&mut self, var: &str, expr: &Expr) -> Option<()> {
        match expr {
            Expr::Binary {
                op: BinaryOp::And,
                left,
                right,
            } => {
                self.emit_bool(var, left)?;
                self.emit_bool(var, right)?;
                self.ops.push(Op::And);
                Some(())
            }
            Expr::Binary {
                op: BinaryOp::Or,
                left,
                right,
            } => {
                self.emit_bool(var, left)?;
                self.emit_bool(var, right)?;
                self.ops.push(Op::Or);
                Some(())
            }
            Expr::Unary {
                op: UnaryOp::Not,
                expr: inner,
            } => {
                self.emit_bool(var, inner)?;
                self.ops.push(Op::Not);
                Some(())
            }
            Expr::Unary {
                op: op @ (UnaryOp::IsNull | UnaryOp::IsNotNull),
                expr: inner,
            } => {
                let (v, key) = var_prop_named(inner)?;
                if v.with_str(|s| s != var) {
                    return None;
                }
                let k = self.intern_key(key)?;
                self.ops.push(Op::LoadProp(k));
                self.ops.push(match op {
                    UnaryOp::IsNull => Op::IsNull,
                    UnaryOp::IsNotNull => Op::IsNotNull,
                    _ => unreachable!(),
                });
                Some(())
            }
            Expr::Binary { op, left, right } if is_pred_op(*op) => {
                // Support both `var.prop <op> const` and mirrored `const <op> var.prop`,
                // as well as two properties of the same node `var.a <op> var.b`.
                if let (Some((lv, lk)), Some((rv, rk))) =
                    (var_prop_named(left), var_prop_named(right))
                {
                    if lv.with_str(|s| s == var) && rv.with_str(|s| s == var) {
                        let lk_idx = self.intern_key(lk)?;
                        let rk_idx = self.intern_key(rk)?;
                        self.ops.push(Op::LoadProp(lk_idx));
                        self.ops.push(Op::LoadProp(rk_idx));
                        self.ops.push(Op::Cmp(*op));
                        return Some(());
                    }
                    return None;
                }
                let ((v, key), bound, oriented_op) = orient(var_prop_named, left, right, *op)?;
                if v.with_str(|s| s != var) || !bound_is_row_invariant(bound) {
                    return None;
                }
                let k = self.intern_key(key)?;
                let c = self.push_const(bound.clone())?;
                self.ops.push(Op::LoadProp(k));
                self.ops.push(Op::LoadConst(c));
                self.ops.push(Op::Cmp(oriented_op));
                Some(())
            }
            _ => None,
        }
    }

    fn finish(self) -> NodeBytecode {
        NodeBytecode {
            ops: self.ops,
            keys: self.keys,
            consts: self.consts,
        }
    }
}
