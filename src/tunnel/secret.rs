//! Serializes the tunnel secret as base64, matching Go's `[]byte` JSON
//! encoding that the quick tunnel service and credentials files use.

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serializer};

pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let encoded = String::deserialize(deserializer)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(serde::de::Error::custom)
}

pub fn serialize<S>(secret: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(secret))
}
