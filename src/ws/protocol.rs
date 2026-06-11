//! Wire protocol (spec §7): `{"v":1,"type":"...","data":{...}}`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default = "default_v")]
    pub v: u8,
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(default)]
    pub data: Value,
}

fn default_v() -> u8 {
    1
}

/// Serialize an outbound message.
pub fn msg(typ: &str, data: Value) -> String {
    serde_json::to_string(&Envelope {
        v: 1,
        typ: typ.to_string(),
        data,
    })
    .expect("envelope serializes")
}

pub fn parse(raw: &str) -> Option<Envelope> {
    serde_json::from_str(raw).ok()
}
