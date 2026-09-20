//! A crate-local, order-preserving JSON value.
//!
//! `serde_json::Value`'s object representation is sorted (`BTreeMap`)
//! unless the crate enables the `preserve_order` feature -- but Cargo
//! unifies features across the whole workspace, so enabling it here would
//! switch every crate's `serde_json::Value` to an insertion-ordered map.
//! Capobara needs insertion order for exactly one thing: `definition_digest`
//! must equal Node's `sha256(JSON.stringify(JSON.parse(text)))`, where key
//! order is the file's order. `OrderedValue` provides that, with hand-written
//! `Deserialize`/`Serialize` impls, at no cost to the rest of the workspace.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Number;

use crate::{Error, Result};

/// A JSON value whose object keys keep the order the deserializer yielded
/// them in. A duplicate key replaces the value stored at the position of
/// its *first* occurrence, matching `JSON.parse`.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderedValue {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<OrderedValue>),
    Object(Vec<(String, OrderedValue)>),
}

impl<'de> Deserialize<'de> for OrderedValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OrderedValueVisitor)
    }
}

struct OrderedValueVisitor;

impl<'de> Visitor<'de> for OrderedValueVisitor {
    type Value = OrderedValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a valid JSON value")
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(OrderedValue::Number)
            .ok_or_else(|| de::Error::custom("invalid floating point number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OrderedValue::String(value))
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(OrderedValue::Array(items))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut entries: Vec<(String, OrderedValue)> =
            Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((key, value)) = map.next_entry::<String, OrderedValue>()? {
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(existing) => existing.1 = value,
                None => entries.push((key, value)),
            }
        }
        Ok(OrderedValue::Object(entries))
    }
}

impl Serialize for OrderedValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            OrderedValue::Null => serializer.serialize_unit(),
            OrderedValue::Bool(value) => serializer.serialize_bool(*value),
            OrderedValue::Number(value) => value.serialize(serializer),
            OrderedValue::String(value) => serializer.serialize_str(value),
            OrderedValue::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            OrderedValue::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// Parses `text` into an order-preserving `OrderedValue` and serializes it
/// back to compact JSON: what `JSON.stringify(JSON.parse(text))` produces
/// for this input class, i.e. object keys stay in the file's order, with a
/// duplicate key resolved to its last value at its first position.
pub fn canonical_compact_json(text: &str) -> Result<String> {
    let value: OrderedValue = serde_json::from_str(text)
        .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))?;
    serde_json::to_string(&value)
        .map_err(|e| Error::Invalid(format!("Invalid projection JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_compact_json_preserves_key_and_array_order() {
        let text = r#"{"b":1, "a":[true,null,"x"], "c":{"z":2,"y":3}}"#;
        assert_eq!(
            canonical_compact_json(text).unwrap(),
            r#"{"b":1,"a":[true,null,"x"],"c":{"z":2,"y":3}}"#
        );
    }

    #[test]
    fn duplicate_keys_keep_the_first_position_with_the_last_value() {
        let text = r#"{"a":1,"b":2,"a":3}"#;
        assert_eq!(canonical_compact_json(text).unwrap(), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn invalid_json_is_reported_as_invalid_projection_json() {
        let err = canonical_compact_json("{not json}")
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Invalid projection JSON"),
            "unexpected error: {err}"
        );
    }
}
