//! Bounded JSON preflight before serde can discard unknown or duplicate keys.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

// Keep in sync with clients/verifier-ts/src/dstack-event-log.ts.
pub(super) const MAX_EVENT_LOG_BYTES: usize = 1024 * 1024;
const MAX_EVENT_LOG_DEPTH: usize = 16;

pub(super) fn preflight(input: &str) -> Result<(), String> {
    if input.len() > MAX_EVENT_LOG_BYTES {
        return Err("dstack event_log exceeds maximum byte size".into());
    }
    let mut deserializer = serde_json::Deserializer::from_str(input);
    CheckedJson { depth: 1 }
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end())
        .map_err(|error| format!("invalid dstack event_log evidence: {error}"))
}

struct CheckedJson {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for CheckedJson {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for CheckedJson {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded dstack event log JSON")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        if self.depth > MAX_EVENT_LOG_DEPTH {
            return Err(de::Error::custom(
                "dstack event_log exceeds maximum nesting depth",
            ));
        }
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate dstack event_log object key"));
            }
            if self.depth == 2 && matches!(key.as_str(), "imr" | "event_type" | "version") {
                // Deserialize directly from the original token: serde's u32
                // rejects floats, exponents, negatives, strings, and overflow.
                let value = map.next_value::<u32>()?;
                if key == "version" && value != 1 {
                    return Err(de::Error::custom("unsupported dstack event log version: expected version absent or numeric 1 (v1)"));
                }
            } else {
                map.next_value_seed(CheckedJson {
                    depth: self.depth + 1,
                })?;
            }
        }
        Ok(())
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        if self.depth > MAX_EVENT_LOG_DEPTH {
            return Err(de::Error::custom(
                "dstack event_log exceeds maximum nesting depth",
            ));
        }
        while seq
            .next_element_seed(CheckedJson {
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _value: &str) -> Result<(), E> {
        // serde_json's decoder already rejects lone surrogate escapes.
        Ok(())
    }

    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}
