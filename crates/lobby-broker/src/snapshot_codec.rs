//! The one JSON codec every persisted-size measurement and the stored snapshot
//! share: `serde_json` with every non-ASCII character written as `\uXXXX`
//! escapes, so the output is pure ASCII.
//!
//! Why ASCII-only: the Worker hands the snapshot to Durable Object
//! `storage.put()` as a JS string, and workerd stores the V8-serialized value.
//! V8 writes a string holding any character above U+00FF at *two bytes per
//! UTF-16 code unit, ASCII included*, so the UTF-8 length of the JSON says
//! nothing about the stored size once a card name contains, say, `Ā`. An
//! ASCII-only string is a one-byte V8 string whose serialized size is its length
//! plus a constant header, so for this codec UTF-8 length == string length ==
//! stored value length (plus the header, which the physical ceiling's margin
//! absorbs). Escaping can inflate the text (up to 6 bytes per UTF-16 code unit),
//! which is why admission must measure the escaped form, and does: every size
//! check in the crate goes through [`encoded_len`].
//!
//! Restore needs nothing special: `\uXXXX` is plain JSON, so
//! `serde_json::from_str` decodes it, and a snapshot written before this codec
//! (raw UTF-8, possibly non-ASCII) is still valid JSON and restores unchanged.
//! Such a snapshot can be *larger* once escaped, so [`stored_snapshot`] keeps the
//! raw form whenever the ASCII form would not fit.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::{Formatter, Serializer};

/// Compact JSON formatter that escapes every non-ASCII character as one `\uXXXX`
/// per UTF-16 code unit (a surrogate pair for characters above U+FFFF), which is
/// how RFC 8259 §7 spells them.
struct AsciiFormatter;

impl Formatter for AsciiFormatter {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut ascii_start = 0;
        for (index, ch) in fragment.char_indices() {
            if ch.is_ascii() {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[ascii_start..index])?;
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                write!(writer, "\\u{unit:04x}")?;
            }
            ascii_start = index + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[ascii_start..])
    }
}

/// `value` as compact, ASCII-only JSON bytes.
pub(crate) fn to_ascii_json_vec<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut out = Vec::new();
    value.serialize(&mut Serializer::with_formatter(&mut out, AsciiFormatter))?;
    Ok(out)
}

/// `value` as an ASCII-only JSON string: the exact text the Durable Object
/// stores.
pub fn to_ascii_json_string<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    let bytes = to_ascii_json_vec(value)?;
    // The serializer emits only valid UTF-8 and this formatter only ASCII.
    String::from_utf8(bytes).map_err(|e| serde::ser::Error::custom(e.to_string()))
}

/// How many bytes the Durable Object stores for a JS string holding `text`
/// (before the V8 serialization framing, which callers reserve for): V8 keeps a
/// string whose every code unit is at most U+00FF as one byte per unit, and any
/// other string at two bytes per UTF-16 code unit, ASCII included.
pub(crate) fn v8_string_bytes(text: &str) -> usize {
    let (units, wide) = text.chars().fold((0usize, false), |(units, wide), ch| {
        (units + ch.len_utf16(), wide || u32::from(ch) > 0xFF)
    });
    if wide {
        2 * units
    } else {
        units
    }
}

/// A snapshot's text and what storing it costs.
pub(crate) struct StoredSnapshot {
    pub(crate) text: String,
    /// Bytes the store holds for `text` (see [`v8_string_bytes`]).
    pub(crate) stored_bytes: usize,
}

/// The one authority for how a snapshot is stored and how big that is.
///
/// Fresh state is written ASCII-only (see the module docs). A snapshot the
/// pre-codec writer stored as raw UTF-8 can be larger once escaped (`Ā` is two
/// bytes stored raw as a two-byte V8 string, six as `Ā`), so when the
/// ASCII form is over `limit` and the raw form stores smaller, the raw form is
/// emitted instead: it is what the store already held, so restoring and
/// re-writing it never produces an unstorable value. Both the text the Worker
/// writes and every capacity measurement come from here, so they cannot drift.
pub(crate) fn stored_snapshot<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> serde_json::Result<StoredSnapshot> {
    let ascii = to_ascii_json_string(value)?;
    if ascii.len() > limit {
        let raw = serde_json::to_string(value)?;
        let raw_bytes = v8_string_bytes(&raw);
        if raw_bytes < ascii.len() {
            return Ok(StoredSnapshot {
                text: raw,
                stored_bytes: raw_bytes,
            });
        }
    }
    let stored_bytes = ascii.len();
    Ok(StoredSnapshot {
        text: ascii,
        stored_bytes,
    })
}

/// Exact byte length of `value` in the stored encoding; [`usize::MAX`] if it
/// cannot be encoded, so every capacity check refuses rather than under-counts.
pub(crate) fn encoded_len<T: Serialize + ?Sized>(value: &T) -> usize {
    to_ascii_json_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_is_escaped_per_utf16_unit_and_round_trips() {
        let original = vec!["plain", "Ā", "Æther \u{1F409} \"q\" \\ \n", "日本"];
        let json = to_ascii_json_string(&original).expect("encodes");
        assert!(json.is_ascii(), "{json}");
        assert!(json.contains("\\u0100"), "BMP char: {json}");
        assert!(json.contains("\\ud83d\\udc09"), "surrogate pair: {json}");
        let back: Vec<String> = serde_json::from_str(&json).expect("decodes");
        assert_eq!(back, original);
        assert_eq!(encoded_len(&original), json.len());
    }

    #[test]
    fn ascii_input_is_byte_identical_to_serde_json() {
        let value = serde_json::json!({"a": ["x", "y\n\"z"], "n": 3, "k\u{7f}": null});
        assert_eq!(
            to_ascii_json_string(&value).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
    }

    #[test]
    fn object_keys_are_escaped_too() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("Ā".to_string(), 1);
        let json = to_ascii_json_string(&map).unwrap();
        assert_eq!(json, r#"{"\u0100":1}"#);
        let back: std::collections::BTreeMap<String, i32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, map);
    }
}
