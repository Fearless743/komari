//! Custom SQL types that Go stores as JSON text.

use std::fmt;
use std::ops::{Deref, DerefMut};

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value, ValueRef};
use serde::{Deserialize, Serialize};

/// JSON-encoded `[]string`, mirroring `models.StringArray` from the Go backend.
///
/// `Value` marshals to a JSON array and `Scan` accepts the text produced by SQLite.
/// SQL `NULL` is treated as an empty array (the Go implementation would error;
/// accepting `NULL` lets us read rows written by other tools without failing).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StringArray(pub Vec<String>);

impl StringArray {
    pub fn new(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(values.into_iter().map(Into::into).collect())
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<String> {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Serialize exactly as Go's `StringArray.Value` does.
    pub fn to_json(&self) -> String {
        // Serializing a `Vec<String>` cannot fail.
        serde_json::to_string(&self.0).expect("string arrays always serialize")
    }

    /// Parse the JSON representation produced by `StringArray.Value`.
    pub fn from_json(value: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(value).map(Self)
    }
}

impl From<Vec<String>> for StringArray {
    fn from(value: Vec<String>) -> Self {
        Self(value)
    }
}

impl From<StringArray> for Vec<String> {
    fn from(value: StringArray) -> Self {
        value.0
    }
}

impl FromIterator<String> for StringArray {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for StringArray {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a StringArray {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Deref for StringArray {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for StringArray {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl AsRef<[String]> for StringArray {
    fn as_ref(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for StringArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

impl ToSql for StringArray {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Text(self.to_json())))
    }
}

impl FromSql for StringArray {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Null => Ok(Self::default()),
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                let text =
                    std::str::from_utf8(bytes).map_err(|err| FromSqlError::Other(Box::new(err)))?;
                Self::from_json(text).map_err(|err| FromSqlError::Other(Box::new(err)))
            }
            other => Err(FromSqlError::Other(Box::new(InvalidStringArray(format!(
                "unsupported SQLite value for StringArray: {other:?}"
            ))))),
        }
    }
}

#[derive(Debug)]
struct InvalidStringArray(String);

impl fmt::Display for InvalidStringArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidStringArray {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_value_matches_go() {
        let array = StringArray::new(["a", "b"]);
        assert_eq!(array.to_json(), r#"["a","b"]"#);
        assert_eq!(array, StringArray::from_json(r#"["a","b"]"#).unwrap());
    }

    #[test]
    fn empty_array_is_not_null() {
        assert_eq!(StringArray::default().to_json(), "[]");
    }
}
