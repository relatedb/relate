//! The grouped aggregate's compact group key.
//!
//! A grouping's key arity is fixed, so a group's key is a fixed-size tuple
//! of [`KeySlot`]s, not a heap `Vec<GroupKey>` of 64-byte trees. A slot is
//! 16 bytes for the dominant keys (null, an integer, a node or
//! relationship id, a boolean, float bits) and boxes the full [`GroupKey`]
//! only for a key that has no word form (strings, lists, maps, temporals,
//! NaN). Equivalence is exactly [`GroupKey`]'s: a slot is derived from
//! [`GroupKey::class`], the one canonical word projection, so two values
//! share a slot exactly when they share a `GroupKey` (a NaN stays a boxed
//! key and keeps its per-occurrence nonce).

use relate_core::{Result, Value};

use crate::group_key::{GroupKey, KeyClass, WordTag};

/// One key part: the normalized equivalence key of one grouping value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum KeySlot {
    Null,
    /// A 64-bit word under its type tag ([`GroupKey::class`]).
    Word(WordTag, u64),
    /// A key only the full tree can represent.
    General(Box<GroupKey>),
}

impl KeySlot {
    /// The slot of `value`. An integer or a node never builds the
    /// intermediate tree: [`GroupKey::class_of`] gives their word directly
    /// (pinned by `fast_paths_agree_with_the_canonical_class`).
    pub(super) fn of(value: &Value) -> Self {
        match GroupKey::class_of(value) {
            Some(KeyClass::Word(tag, word)) => Self::Word(tag, word),
            _ => Self::from_key(GroupKey::new(value)),
        }
    }

    fn from_key(key: GroupKey) -> Self {
        match key.class() {
            KeyClass::Null => Self::Null,
            KeyClass::Word(tag, word) => Self::Word(tag, word),
            KeyClass::General => Self::General(Box::new(key)),
        }
    }

    /// The heap this slot owns beyond its inline 16 bytes.
    fn heap_bytes(&self) -> u64 {
        match self {
            Self::Null | Self::Word(..) => 0,
            Self::General(key) => size_of::<GroupKey>() as u64 + key.logical_bytes(),
        }
    }
}

/// The grouping values a [`KeyTuple`] was built from. The tuple is the
/// normalized equivalence key; these are the first-seen values the output
/// row needs.
pub(super) enum KeyValues {
    One(Value),
    Many(Vec<Value>),
}

impl KeyValues {
    pub(super) fn get(&self, position: usize) -> &Value {
        match self {
            Self::One(value) => value,
            Self::Many(values) => &values[position],
        }
    }

    pub(super) fn take(&mut self, position: usize) -> Value {
        match self {
            Self::One(value) => std::mem::take(value),
            Self::Many(values) => std::mem::take(&mut values[position]),
        }
    }
}

/// A group's key: arity 1 inline, any other arity one boxed slice (arity 0
/// is the keyless group's empty slice, which allocates nothing).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum KeyTuple {
    One(KeySlot),
    Many(Box<[KeySlot]>),
}

impl KeyTuple {
    /// The key of one input row, with the values it was built from:
    /// `value(i)` is the `i`th grouping value.
    pub(super) fn of(
        arity: usize,
        mut value: impl FnMut(usize) -> Result<Value>,
    ) -> Result<(Self, KeyValues)> {
        if arity == 1 {
            let value = value(0)?;
            return Ok((Self::One(KeySlot::of(&value)), KeyValues::One(value)));
        }
        let values = (0..arity).map(&mut value).collect::<Result<Vec<_>>>()?;
        let slots = values.iter().map(KeySlot::of).collect();
        Ok((Self::Many(slots), KeyValues::Many(values)))
    }

    /// Bytes the key owns beyond the index entry that holds it: the boxed
    /// slice of a multi-part key and every boxed tree.
    pub(super) fn heap_bytes(&self) -> u64 {
        match self {
            Self::One(slot) => slot.heap_bytes(),
            // A boxed slice has no header: exactly its slots.
            Self::Many(slots) => {
                (slots.len() * size_of::<KeySlot>()) as u64
                    + slots.iter().map(KeySlot::heap_bytes).sum::<u64>()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::node_value as node;

    #[test]
    fn fast_paths_agree_with_the_canonical_class() {
        for value in [Value::Int(7), Value::Int(-1), node(3), node(0)] {
            assert!(
                KeySlot::of(&value) == KeySlot::from_key(GroupKey::new(&value)),
                "{value:?}"
            );
        }
    }

    #[test]
    fn slots_follow_group_key_equivalence() {
        assert!(KeySlot::of(&Value::Int(1)) == KeySlot::of(&Value::Float(1.0)));
        assert!(KeySlot::of(&Value::Float(0.0)) == KeySlot::of(&Value::Float(-0.0)));
        assert!(KeySlot::of(&Value::Null) == KeySlot::of(&Value::Null));
        assert!(KeySlot::of(&Value::Int(1)) != KeySlot::of(&node(1)));
        assert!(KeySlot::of(&node(1)) == KeySlot::of(&node(1)));
        let nan = Value::Float(f64::NAN);
        assert!(
            KeySlot::of(&nan) != KeySlot::of(&nan),
            "every NaN is its own key"
        );
        let text = |s: &str| Value::String(s.into());
        assert!(KeySlot::of(&text("a")) == KeySlot::of(&text("a")));
        assert!(KeySlot::of(&text("a")) != KeySlot::of(&text("b")));
    }

    #[test]
    fn a_word_key_owns_no_heap_and_a_slot_is_sixteen_bytes() {
        assert_eq!(size_of::<KeySlot>(), 16);
        let tuple = |values: Vec<Value>| {
            KeyTuple::of(values.len(), |i| Ok(values[i].clone()))
                .expect("a key")
                .0
        };
        assert_eq!(tuple(vec![node(9)]).heap_bytes(), 0);
        assert!(tuple(vec![Value::String("abc".into())]).heap_bytes() > 0);
        let pair = tuple(vec![Value::Int(1), Value::Int(2)]);
        assert!(pair.heap_bytes() >= 2 * size_of::<KeySlot>() as u64);
    }
}
