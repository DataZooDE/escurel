//! Null-tolerant deserialization helper.
//!
//! The MCP wire frequently emits an explicit JSON `null` for an
//! empty/absent scalar (e.g. an inbox event's `instance_page_id`, a
//! parsed wikilink's `anchor`/`version`, a paginated read's
//! `next_cursor`). `#[serde(default)]` only fills *missing* keys — it
//! does **not** turn an explicit `null` into the type default — so a
//! `String`/`Vec`/… field that receives `null` would otherwise fail to
//! decode with "invalid type: null, expected a string".
//!
//! Use `#[serde(default, deserialize_with = "null::null_as_default")]`
//! on any field whose wire value may be an explicit `null` but whose
//! Rust type is a non-`Option` `Default`.

use serde::{Deserialize, Deserializer};

/// Deserialize `T`, mapping an explicit JSON `null` to `T::default()`.
pub fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let opt = Option::<T>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}
