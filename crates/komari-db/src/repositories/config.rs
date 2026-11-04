//! Key/value configuration repository (port of the Go `config` package).

use std::collections::HashMap;

use rusqlite::params;
use serde::Deserializer;
use serde::Serialize;
use serde::de::{self, DeserializeOwned, DeserializeSeed, SeqAccess, Visitor};
use serde_json::Value as JsonValue;

use super::{query_all, query_optional};
use crate::db::Db;
use crate::error::Result;
use crate::models::ConfigItem;

/// Access to the `configs` key/value table.
pub struct ConfigRepo<'a> {
    pub(crate) db: &'a Db,
}

impl ConfigRepo<'_> {
    /// Raw JSON text for a key.
    pub fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let conn = self.db.conn();
        let item: Option<ConfigItem> =
            query_optional(&conn, "SELECT * FROM configs WHERE key = ?1", [key])?;
        Ok(item.map(|item| item.value))
    }

    /// Store raw JSON text for a key.
    pub fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO configs (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Decode a value as generic JSON (Go `config.Get`).
    pub fn get(&self, key: &str) -> Result<Option<JsonValue>> {
        match self.get_raw(key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// Decode a value, writing `default` when the key is absent
    /// (Go `config.Get` with a default argument).
    pub fn get_or(&self, key: &str, default: JsonValue) -> Result<JsonValue> {
        if let Some(value) = self.get(key)? {
            return Ok(value);
        }
        self.set_value(key, &default)?;
        Ok(default)
    }

    /// Typed decode (Go `config.GetAs`). Returns `None` when the key is absent.
    ///
    /// Values are decoded with the same fallbacks as Go: JSON `null` becomes
    /// the type's zero value and numbers are converted between integer and
    /// floating-point targets (a stored `80.0` can be read as `i64`).
    pub fn get_as<T: DeserializeOwned + Default>(&self, key: &str) -> Result<Option<T>> {
        match self.get_raw(key)? {
            Some(raw) => {
                let value: JsonValue = serde_json::from_str(&raw)?;
                Ok(Some(decode_loose(&value)?))
            }
            None => Ok(None),
        }
    }

    /// Typed decode with a default that is persisted when the key is absent
    /// (Go `config.GetAs` with a default argument).
    pub fn get_as_or<T: DeserializeOwned + Serialize + Default>(
        &self,
        key: &str,
        default: T,
    ) -> Result<T> {
        if let Some(raw) = self.get_raw(key)? {
            let value: JsonValue = serde_json::from_str(&raw)?;
            return decode_loose(&value);
        }
        self.set(key, &default)?;
        Ok(default)
    }

    /// Serialize and store a value as compact JSON.
    pub fn set<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        self.set_raw(key, &raw)
    }

    /// Store an already-decoded JSON value.
    pub fn set_value(&self, key: &str, value: &JsonValue) -> Result<()> {
        self.set(key, value)
    }

    /// Remove a key. Returns `true` when a row was deleted.
    pub fn remove(&self, key: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM configs WHERE key = ?1", [key])? > 0)
    }

    pub fn contains(&self, key: &str) -> Result<bool> {
        let conn = self.db.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM configs WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// All configuration entries whose values are valid JSON (Go
    /// `config.GetAll`); invalid rows are skipped.
    pub fn get_all(&self) -> Result<HashMap<String, JsonValue>> {
        let conn = self.db.conn();
        let items: Vec<ConfigItem> = query_all(&conn, "SELECT * FROM configs", [])?;
        let mut result = HashMap::with_capacity(items.len());
        for item in items {
            if let Ok(value) = serde_json::from_str(&item.value) {
                result.insert(item.key, value);
            }
        }
        Ok(result)
    }

    /// Fetch several values at once. Missing keys are omitted.
    pub fn get_many(&self, keys: &[&str]) -> Result<HashMap<String, JsonValue>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (1..=keys.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let conn = self.db.conn();
        let items: Vec<ConfigItem> = query_all(
            &conn,
            &format!("SELECT * FROM configs WHERE key IN ({placeholders})"),
            rusqlite::params_from_iter(keys.iter()),
        )?;
        let mut result = HashMap::with_capacity(items.len());
        for item in items {
            if let Ok(value) = serde_json::from_str(&item.value) {
                result.insert(item.key, value);
            }
        }
        Ok(result)
    }

    /// Upsert many values in a single transaction (Go `config.SetMany`).
    pub fn set_many(&self, values: &HashMap<String, JsonValue>) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO configs (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?;
            for (key, value) in values {
                stmt.execute(params![key, serde_json::to_string(value)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Number of configuration rows.
    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM configs", [], |row| row.get(0))?;
        Ok(count)
    }
}

/// Decode a JSON value with Go `config.GetAs`-style fallbacks.
fn decode_loose<T: DeserializeOwned + Default>(value: &JsonValue) -> Result<T> {
    // `json.Unmarshal("null", &t)` leaves t at its zero value and returns nil.
    if value.is_null() {
        return Ok(T::default());
    }
    match T::deserialize(GoValue(value.clone())) {
        Ok(decoded) => Ok(decoded),
        Err(_) => {
            // Go's fallback decodes into `any` (numbers become float64), then
            // re-marshals that generic value and unmarshals it into T. The
            // remarshal turns integral float64 values such as 80.0 into `80`,
            // which lets them populate integer fields inside structs/slices/
            // maps. Non-integral values and out-of-range narrow integers still
            // fail at the final strict unmarshal.
            let generic = go_generic_value(value);
            let remarshaled = go_remarshal_value(&generic);
            Ok(T::deserialize(remarshaled)?)
        }
    }
}

/// Convert a stored JSON value to what Go's `json.Unmarshal(..., &any)` would
/// produce: every number becomes an `f64`, arrays/maps recurse.
fn go_generic_value(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Number(number) => {
            let float = number.as_f64().unwrap_or_default();
            JsonValue::Number(serde_json::Number::from_f64(float).unwrap_or_else(|| 0.into()))
        }
        JsonValue::Array(values) => JsonValue::Array(values.iter().map(go_generic_value).collect()),
        JsonValue::Object(values) => JsonValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), go_generic_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Approximate `json.Marshal(any)` followed by `json.Unmarshal(..., &T)`:
/// integral floats are emitted as integers when they fit, while fractions and
/// out-of-range values stay floating point.
fn go_remarshal_value(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Number(number) => {
            let Some(float) = number.as_f64() else {
                return value.clone();
            };
            if float.is_finite() && float.fract() == 0.0 {
                if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&float) {
                    return JsonValue::from(float as i64);
                }
                if (0.0..18_446_744_073_709_551_616.0).contains(&float) {
                    return JsonValue::from(float as u64);
                }
            }
            JsonValue::Number(serde_json::Number::from_f64(float).unwrap_or_else(|| 0.into()))
        }
        JsonValue::Array(values) => {
            JsonValue::Array(values.iter().map(go_remarshal_value).collect())
        }
        JsonValue::Object(values) => JsonValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), go_remarshal_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Top-level deserializer mirroring Go's `convertAndSet`.
///
/// Go first tries `json.Unmarshal` into `T`, then falls back to decoding the
/// value as `any` (so JSON numbers become `float64`) and applying reflect-style
/// conversions. Scalar numeric targets reach `reflect.SetInt`/`SetUint`; the
/// methods below reproduce those conversions. Compound targets keep serde's
/// normal deserialization.
struct GoValue(JsonValue);

/// Convert a JSON number to `int64` the way Go does on amd64.
///
/// In-range floats truncate toward zero. Out-of-range values (and NaN) become
/// `i64::MIN`, which is what Go's `int64(float64)` produces for values that do
/// not fit.
fn go_float_to_i64(value: f64) -> i64 {
    let truncated = value.trunc();
    if truncated.is_nan()
        || truncated < -9_223_372_036_854_775_808.0
        || truncated >= 9_223_372_036_854_775_808.0
    {
        i64::MIN
    } else {
        truncated as i64
    }
}

/// Convert a JSON number to `uint64` the way Go does on amd64.
///
/// Negative in-range values wrap through their `int64` representation (so
/// `-1.0` becomes `u64::MAX`); values outside `[0, 2^64)` become `2^63`, the
/// same result as Go's `uint64(float64)` for those inputs.
fn go_float_to_u64(value: f64) -> u64 {
    if value < 9_223_372_036_854_775_808.0 {
        go_float_to_i64(value) as u64
    } else if value < 18_446_744_073_709_551_616.0 {
        value.trunc() as u64
    } else {
        1_u64 << 63
    }
}

fn go_i64(value: &JsonValue) -> Option<i64> {
    let number = value.as_number()?;
    if let Some(integer) = number.as_i64() {
        return Some(integer);
    }
    if let Some(integer) = number.as_u64() {
        return Some(go_float_to_i64(integer as f64));
    }
    Some(go_float_to_i64(number.as_f64()?))
}

fn go_u64(value: &JsonValue) -> Option<u64> {
    let number = value.as_number()?;
    if let Some(integer) = number.as_u64() {
        return Some(integer);
    }
    if let Some(integer) = number.as_i64() {
        return Some(go_float_to_u64(integer as f64));
    }
    Some(go_float_to_u64(number.as_f64()?))
}

fn go_bytes(value: &JsonValue) -> Option<Vec<u8>> {
    let text = value.as_str()?;
    // Go's `json.Unmarshal` base64-decodes strings into `[]byte` using
    // `base64.StdEncoding`: CR/LF are ignored and non-zero trailing bits are
    // accepted. When decoding genuinely fails, `convertAndSet` reaches the
    // reflect string->[]byte conversion and keeps the raw bytes.
    Some(go_base64_decode(text).unwrap_or_else(|| text.as_bytes().to_vec()))
}

fn go_base64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::alphabet;
    use base64::engine::Engine;
    use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};

    let cleaned: String = text
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .collect();
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );
    engine.decode(cleaned.as_bytes()).ok()
}

/// Sequence access for a base64/raw string decoded into bytes. Only `u8`
/// elements are accepted, so `Vec<u8>` succeeds while `Vec<i64>` and other
/// sequence targets still reject the string like Go's remarshal fallback.
struct ByteSeqAccess {
    bytes: std::vec::IntoIter<u8>,
}

impl<'de> SeqAccess<'de> for ByteSeqAccess {
    type Error = serde_json::Error;

    fn next_element_seed<T>(
        &mut self,
        seed: T,
    ) -> std::result::Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(byte) = self.bytes.next() else {
            return Ok(None);
        };
        seed.deserialize(ByteValue(byte)).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.bytes.len())
    }
}

struct ByteValue(u8);

impl<'de> Deserializer<'de> for ByteValue {
    type Error = serde_json::Error;

    fn deserialize_u8<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u8(self.0)
    }

    fn deserialize_any<V>(self, _visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(de::Error::custom("expected a byte value"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

macro_rules! go_signed {
    ($method:ident, $visit:ident, $ty:ty, $convert:ident) => {
        fn $method<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match $convert(&self.0) {
                Some(value) => visitor.$visit(value as $ty),
                None => self.0.$method(visitor),
            }
        }
    };
}

macro_rules! go_unsigned {
    ($method:ident, $visit:ident, $ty:ty, $convert:ident) => {
        fn $method<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match $convert(&self.0) {
                Some(value) => visitor.$visit(value as $ty),
                None => self.0.$method(visitor),
            }
        }
    };
}

/// Narrow integers go through Go's generic `any` path, where every JSON number
/// is a `float64` first. That matters for values outside `i64`/`u64` range.
fn go_narrow_i64(value: &JsonValue) -> Option<i64> {
    value.as_f64().map(go_float_to_i64)
}

fn go_narrow_u64(value: &JsonValue) -> Option<u64> {
    value.as_f64().map(go_float_to_u64)
}

impl<'de> Deserializer<'de> for GoValue {
    type Error = serde_json::Error;

    go_signed!(deserialize_i8, visit_i8, i8, go_narrow_i64);
    go_signed!(deserialize_i16, visit_i16, i16, go_narrow_i64);
    go_signed!(deserialize_i32, visit_i32, i32, go_narrow_i64);
    go_signed!(deserialize_i64, visit_i64, i64, go_i64);
    go_signed!(deserialize_i128, visit_i128, i128, go_i64);

    go_unsigned!(deserialize_u8, visit_u8, u8, go_narrow_u64);
    go_unsigned!(deserialize_u16, visit_u16, u16, go_narrow_u64);
    go_unsigned!(deserialize_u32, visit_u32, u32, go_narrow_u64);
    go_unsigned!(deserialize_u64, visit_u64, u64, go_u64);
    go_unsigned!(deserialize_u128, visit_u128, u128, go_u64);

    fn deserialize_f32<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0.as_f64() {
            Some(value) => visitor.visit_f32(value as f32),
            None => self.0.deserialize_f32(visitor),
        }
    }

    fn deserialize_f64<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0.as_f64() {
            Some(value) => visitor.visit_f64(value),
            None => self.0.deserialize_f64(visitor),
        }
    }

    fn deserialize_seq<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        // Go's `encoding/json` decodes a base64 string into `[]byte`. The
        // byte-only SeqAccess keeps other sequence targets (`Vec<i64>`,
        // `Vec<String>`, ...) from misreading the string as bytes.
        if let Some(bytes) = go_bytes(&self.0) {
            return visitor.visit_seq(ByteSeqAccess {
                bytes: bytes.into_iter(),
            });
        }
        self.0.deserialize_seq(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match go_bytes(&self.0) {
            Some(bytes) => visitor.visit_byte_buf(bytes),
            None => self.0.deserialize_bytes(visitor),
        }
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match go_bytes(&self.0) {
            Some(bytes) => visitor.visit_byte_buf(bytes),
            None => self.0.deserialize_byte_buf(visitor),
        }
    }

    fn deserialize_any<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_any(visitor)
    }

    fn deserialize_bool<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_bool(visitor)
    }

    fn deserialize_char<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_char(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_str(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_string(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_option(visitor)
    }

    fn deserialize_unit<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_unit(visitor)
    }

    fn deserialize_unit_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_map(visitor)
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_enum(name, variants, visitor)
    }

    fn deserialize_tuple<V>(
        self,
        len: usize,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_identifier(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_ignored_any(visitor)
    }
}
