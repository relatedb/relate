//! The Query API v2 reply envelope: the wire model (ADR-0232 §3, ADR-0386
//! §3) every reply body serializes through.
//!
//! A reply body is one insertion-ordered list of top-level members
//! ([`ReplyBody`]): the success envelope — `data` first (`fields`, then the
//! materialized rows), then the tail its one constructor builds — and every
//! whole-object reply (the error envelopes, the `GET /` discovery document, a
//! statement-less transaction answer) are the same type through the same
//! [`Serialize`] impl, so a below-threshold reply's bytes are exactly the
//! bytes a streamed one carries. The list, not a `serde_json` map, is the
//! body: member order is the wire contract, so a whole-object reply serializes
//! in the order its members were inserted, never in a map's key order.
//!
//! [`ReplyBody::insert_member`] is the body's one mutator: a present key is
//! replaced in place, a missing key is appended. This module is the wire shape
//! only — `crate::stream` owns the framing that carries it.

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::Value as Json;

use crate::v2_json::{v2_value_json, Form};

/// One top-level member's value: an already-built JSON tree, or the success
/// envelope's `data` block, whose rows stay the engine's own values and are
/// encoded one at a time as the writer walks them (ADR-0675) — never rebuilt
/// as a body-sized JSON tree before the first byte goes out.
pub(crate) enum Member {
    /// A value built as JSON up front: every whole-object reply and the
    /// success envelope's small tail members.
    Json(Json),
    /// The success envelope's `data`: `fields`, then the rows.
    Data(DataBlock),
}

impl From<Json> for Member {
    fn from(value: Json) -> Self {
        Member::Json(value)
    }
}

impl Serialize for Member {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Member::Json(value) => value.serialize(serializer),
            Member::Data(data) => data.serialize(serializer),
        }
    }
}

/// The success envelope's `data` object (ADR-0386 §3): `fields`, then
/// `values`. The rows are the engine's materialized values, encoded per
/// value inside [`Serialize`], so the transient JSON a row needs lives for
/// that row alone (ADR-0675). Member order is fixed here — `fields` first —
/// not by a map's insertion order.
pub(crate) struct DataBlock {
    fields: Vec<String>,
    rows: Vec<Vec<relate::Value>>,
    /// The negotiated form and the statement's routed namespace, the same
    /// [`Form`] the encoder entry takes: every encoded entity's element id
    /// comes from the namespace (ADR-0554), and a namespace without an
    /// element-id segment has already been refused before the writer starts.
    form: Form,
}

impl DataBlock {
    pub(crate) fn new(fields: Vec<String>, rows: Vec<Vec<relate::Value>>, form: Form) -> Self {
        Self { fields, rows, form }
    }
}

impl Serialize for DataBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut data = serializer.serialize_map(Some(2))?;
        data.serialize_entry("fields", &self.fields)?;
        data.serialize_entry(
            "values",
            &RowsView {
                rows: &self.rows,
                form: self.form,
            },
        )?;
        data.end()
    }
}

/// The streamed success envelope (ADR-0682): the wire shape a collected
/// [`ReplyBody`] carries — `{"data":{"fields":[…],"values":[` the rows
/// `]}`, then the tail members, then `}` — written piece by piece as the
/// engine's batches arrive, so a streamed body is byte for byte the body the
/// collected reply would have been. Each piece fails as a
/// [`serde_json::Error`]; a failed write is its `io` class
/// ([`serde_json::Error::is_io`]).
pub(crate) mod streamed {
    use std::io::Write;

    use super::{Member, RowView};
    use crate::v2_json::Form;

    /// `{"data":{"fields":[…],"values":[` — the envelope up to its first row.
    pub(crate) fn open(out: &mut impl Write, fields: &[String]) -> serde_json::Result<()> {
        out.write_all(br#"{"data":{"fields":"#)
            .map_err(serde_json::Error::io)?;
        serde_json::to_writer(&mut *out, fields)?;
        out.write_all(br#","values":["#)
            .map_err(serde_json::Error::io)
    }

    /// Append `rows` to the `values` array, each after a comma unless it is
    /// the array's first (`wrote_row` carries that across batches).
    pub(crate) fn rows(
        out: &mut impl Write,
        wrote_row: &mut bool,
        rows: &[Vec<relate::Value>],
        form: Form,
    ) -> serde_json::Result<()> {
        for row in rows {
            if std::mem::replace(wrote_row, true) {
                out.write_all(b",").map_err(serde_json::Error::io)?;
            }
            serde_json::to_writer(&mut *out, &RowView::new(row, form))?;
        }
        Ok(())
    }

    /// Close the `values` array and the `data` block, write the tail
    /// `members` in order (the success tail, or the in-band `errors`
    /// member), and close the envelope.
    pub(crate) fn close(
        out: &mut impl Write,
        members: &[(String, Member)],
    ) -> serde_json::Result<()> {
        out.write_all(b"]}").map_err(serde_json::Error::io)?;
        for (key, member) in members {
            out.write_all(b",").map_err(serde_json::Error::io)?;
            serde_json::to_writer(&mut *out, key)?;
            out.write_all(b":").map_err(serde_json::Error::io)?;
            serde_json::to_writer(&mut *out, member)?;
        }
        out.write_all(b"}").map_err(serde_json::Error::io)
    }
}

/// The `values` array: each row an array of its v2-encoded values.
struct RowsView<'a> {
    rows: &'a [Vec<relate::Value>],
    form: Form,
}

impl Serialize for RowsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.rows.len()))?;
        for row in self.rows {
            rows.serialize_element(&RowView {
                row,
                form: self.form,
            })?;
        }
        rows.end()
    }
}

/// One row as an array of its v2-encoded values — the element of the
/// `values` array, and what the statement streamer writes per row.
pub(crate) struct RowView<'a> {
    row: &'a [relate::Value],
    form: Form,
}

impl<'a> RowView<'a> {
    pub(crate) fn new(row: &'a [relate::Value], form: Form) -> Self {
        Self { row, form }
    }
}

impl Serialize for RowView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut values = serializer.serialize_seq(Some(self.row.len()))?;
        for value in self.row {
            // A value the wire cannot carry was refused before the writer
            // started (`success_envelope`); reaching it here is a
            // serialization failure, never a silent fallback.
            let encoded = v2_value_json(&relate::value_to_cypher(value), self.form)
                .map_err(serde::ser::Error::custom)?;
            values.serialize_element(&encoded)?;
        }
        values.end()
    }
}

/// A reply body (ADR-0386 §3/§9): its top-level members in wire order.
pub struct ReplyBody(Vec<(String, Member)>);

impl ReplyBody {
    /// The one constructor: the top-level members in the order they
    /// serialize — `data` first for a success envelope
    /// (`crate::reply::success_envelope`), constructor order for a whole-object
    /// reply.
    pub(crate) fn new<S: Into<String>, V: Into<Member>>(
        members: impl IntoIterator<Item = (S, V)>,
    ) -> Self {
        Self(
            members
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    /// The member-less body — the 202 rollback answer, `{}` on the wire.
    pub(crate) fn empty() -> Self {
        Self(Vec::new())
    }

    /// Insert one top-level member with JSON-object semantics: a member whose
    /// key is already present is replaced where it stands, a missing one is
    /// appended after the last member. The one mutation path of a reply body,
    /// so no key can appear twice.
    pub(crate) fn insert_member(&mut self, key: &str, value: Json) {
        let value = Member::Json(value);
        match self.0.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, existing)) => *existing = value,
            None => self.0.push((key.to_string(), value)),
        }
    }
}

impl Serialize for ReplyBody {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut body = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            body.serialize_entry(key, value)?;
        }
        body.end()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::reply::Reply;

    /// The whole-body view of a reply for assertions: the body serializes
    /// through the seam's one `Serialize` impl. Test-only — it lives beside
    /// the body type it reads, not in a route family's test module.
    pub(crate) fn body_json(reply: &Reply) -> serde_json::Value {
        serde_json::to_value(&reply.body).expect("a reply body serializes to JSON")
    }

    use serde_json::json;

    use super::*;
    use crate::v2_json::ResponseMediaType;

    /// The lazy `data` block writes the exact bytes the JSON tree it replaced
    /// did, in both encodings: `fields` first, rows as arrays of v2 values.
    #[test]
    fn the_data_block_serializes_like_the_json_tree_it_replaced() {
        let rows = vec![
            vec![relate::Value::Int(1), relate::Value::Null],
            vec![relate::Value::String("a".into()), relate::Value::Bool(true)],
        ];
        for typed in [false, true] {
            let form = Form {
                media_type: if typed {
                    ResponseMediaType::Neo4jQuery
                } else {
                    ResponseMediaType::Json
                },
                namespace: Some(relate::Namespace::Neo4j),
            };
            let tree: Vec<Json> = rows
                .iter()
                .map(|row| {
                    Json::Array(
                        row.iter()
                            .map(|v| {
                                v2_value_json(&relate::value_to_cypher(v), form)
                                    .expect("a graph namespace encodes")
                            })
                            .collect(),
                    )
                })
                .collect();
            let expected = serde_json::to_string(&json!({
                "data": {"fields": ["a", "b"], "values": tree}
            }))
            .unwrap();
            let body = ReplyBody::new([(
                "data",
                Member::Data(DataBlock::new(
                    vec!["a".into(), "b".into()],
                    rows.clone(),
                    form,
                )),
            )]);
            assert_eq!(
                serde_json::to_string(&body).unwrap(),
                expected,
                "typed={typed}"
            );
        }
    }

    /// The one insertion implementation: an existing key keeps its place (the
    /// tx routes re-inserting a tail member), a new key arrives last (their
    /// `transaction` block), and the body never carries one key twice.
    #[test]
    fn insert_member_replaces_in_place_and_appends_new_members() {
        let mut body = ReplyBody::new([
            ("data", json!({"fields": [], "values": []})),
            ("counters", json!({"nodes_created": 0})),
            ("bookmarks", json!([])),
        ]);
        body.insert_member("counters", json!({"nodes_created": 1}));
        body.insert_member("transaction", json!({"id": "t1"}));
        let text = serde_json::to_string(&body).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"data":{"fields":[],"values":[]},"#,
                r#""counters":{"nodes_created":1},"bookmarks":[],"#,
                r#""transaction":{"id":"t1"}}"#
            )
        );
        assert_eq!(
            text.matches(r#""counters":"#).count(),
            1,
            "the replaced member appears once: {text}"
        );
    }

    /// A whole-object body serializes in insertion order — the order its
    /// caller inserted the members, never a map's key order (which would sort
    /// these the other way round).
    #[test]
    fn a_whole_object_body_serializes_in_insertion_order() {
        let body = ReplyBody::new([
            ("second", json!(2)),
            ("first", json!(1)),
            ("third", json!(3)),
        ]);
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"second":2,"first":1,"third":3}"#
        );
    }
}
