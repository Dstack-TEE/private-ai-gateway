// SPDX-FileCopyrightText: © 2025 Phala Network <dstack@phala.network>
// SPDX-License-Identifier: Apache-2.0
//
// Port of Dstack-TEE/dstack@verifier-v0.6.3 dstack/cc-eventlog/src/{tdx,runtime_events}.rs.
// Upstream is a dev-only parity oracle: its serde_jcs dependency enables
// serde_json/float_roundtrip, breaking desktop connection-config recovery.
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha384};

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x08000001;

/// Upstream TdxEvent's JSON wire shape. Byte fields serialize as hex strings.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct DstackEventLog {
    pub imr: u32,
    pub event_type: u32,
    #[serde(default, with = "hex_bytes")]
    pub digest: Vec<u8>,
    pub event: String,
    #[serde(with = "hex_bytes")]
    pub event_payload: Vec<u8>,
    #[serde(
        default = "v1",
        deserialize_with = "version",
        skip_serializing_if = "is_v1"
    )]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
}

fn v1() -> u32 {
    1
}

fn is_v1(version: &u32) -> bool {
    *version == 1
}

fn version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    match version {
        1 | 2 => Ok(version),
        _ => Err(serde::de::Error::custom(format!(
            "unknown event log version: {version}"
        ))),
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        hex::decode(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl DstackEventLog {
    pub fn is_runtime_event(&self) -> bool {
        self.event_type == DSTACK_RUNTIME_EVENT_TYPE
    }

    fn runtime_preimage(&self) -> Result<Vec<u8>, String> {
        match self.version {
            1 => {
                let mut bytes = DSTACK_RUNTIME_EVENT_TYPE.to_le_bytes().to_vec();
                bytes.push(b':');
                bytes.extend_from_slice(self.event.as_bytes());
                bytes.push(b':');
                bytes.extend_from_slice(&self.event_payload);
                Ok(bytes)
            }
            2 => {
                // Fixed ASCII keys, strings and a u32: the existing helper
                // produces exactly upstream's JCS bytes without float parsing.
                aci_protocol::digest::jcs_bytes(&serde_json::json!({
                    "name": self.event,
                    "payload": hex::encode(&self.event_payload),
                    "type": DSTACK_RUNTIME_EVENT_TYPE,
                }))
                .map_err(|error| error.to_string())
            }
            version => Err(format!("unknown event log version: {version}")),
        }
    }
}

fn validate_v2_preimages(events: &[DstackEventLog]) -> Result<(), String> {
    for (index, event) in events.iter().enumerate() {
        if !event.is_runtime_event() || event.version != 2 {
            continue;
        }
        let supplied = event
            .preimage
            .as_deref()
            .ok_or_else(|| format!("V2 runtime event {index} is missing its digest preimage"))?;
        let supplied = hex::decode(supplied)
            .map_err(|_| format!("V2 runtime event {index} has a malformed digest preimage"))?;
        let advertised: [u8; 48] = Sha384::digest(&supplied).into();
        if advertised.as_slice() != event.digest {
            return Err(format!(
                "V2 runtime event {index} digest does not match its preimage"
            ));
        }
        if supplied != event.runtime_preimage()? {
            return Err(format!(
                "V2 runtime event {index} preimage is not the canonical event representation"
            ));
        }
    }
    Ok(())
}

pub(super) fn replay_rtmr3(events: &[DstackEventLog]) -> Result<[u8; 48], String> {
    validate_v2_preimages(events)?;
    let mut mr = [0u8; 48];
    for (index, event) in events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.imr == 3)
    {
        // Non-runtime TdxEvent payloads have no reconstructable preimage.
        if !event.is_runtime_event() {
            return Err(format!("dstack RTMR3 event {index} is not a runtime event"));
        }
        let digest: [u8; 48] = Sha384::digest(event.runtime_preimage()?).into();
        if digest.as_slice() != event.digest {
            return Err(format!(
                "dstack runtime event {index} digest does not match its fields"
            ));
        }
        mr = Sha384::digest([mr.as_slice(), digest.as_slice()].concat()).into();
    }
    Ok(mr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_eventlog::{EventLogVersion, RuntimeEvent, TdxEvent};
    use serde_json::Value;

    #[test]
    fn port_matches_upstream_vectors_and_generated_edge_events() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../clients/verifier-ts/test/fixtures/dstack_event_vectors.json"
        ))
        .unwrap();
        for vector in fixture["vectors"].as_array().unwrap() {
            let upstream: Vec<TdxEvent> = serde_json::from_value(vector["events"].clone()).unwrap();
            let port: Vec<DstackEventLog> =
                serde_json::from_value(vector["events"].clone()).unwrap();
            assert_eq!(
                validate_v2_preimages(&port).is_ok(),
                cc_eventlog::tdx::validate_v2_preimages(&upstream).is_ok(),
                "{}",
                vector["name"]
            );
            if let Ok(replayed) = replay_rtmr3(&port) {
                let runtime: Vec<_> = upstream
                    .iter()
                    .filter(|event| event.imr == 3)
                    .filter_map(TdxEvent::to_runtime_event)
                    .collect();
                assert_eq!(
                    replayed,
                    cc_eventlog::replay_events::<ez_hash::Sha384>(&runtime, None)
                );
            }
            for (ours, theirs) in port.iter().zip(&upstream) {
                if let Some(runtime) = theirs.to_runtime_event() {
                    assert_eq!(ours.runtime_preimage().unwrap(), runtime.preimage());
                    let digest: [u8; 48] = Sha384::digest(ours.runtime_preimage().unwrap()).into();
                    assert_eq!(digest, runtime.sha384_digest());
                }
            }
        }
        let edges = [
            "",
            ":",
            "a:b",
            "\0\u{1}\u{8}\u{c}\n\r\t\"\\",
            "测试-🦀",
            "\u{2028}\u{2029}",
        ];
        let mut runtime = Vec::new();
        for i in 0..128u32 {
            // Deterministic pseudorandom payloads/names without a test dependency.
            let payload = sha2::Sha256::digest(i.to_le_bytes()).to_vec();
            let name = format!(
                "{}{}",
                edges[i as usize % edges.len()],
                hex::encode(&payload[..4])
            );
            for version in [EventLogVersion::V1, EventLogVersion::V2] {
                runtime.push(RuntimeEvent::new(name.clone(), payload.clone(), version));
            }
        }
        for name in edges {
            for version in [EventLogVersion::V1, EventLogVersion::V2] {
                runtime.push(RuntimeEvent::new(name.into(), vec![], version));
            }
        }
        let mut upstream: Vec<TdxEvent> = runtime.iter().cloned().map(Into::into).collect();
        cc_eventlog::tdx::fill_v2_preimages(&mut upstream);
        let port: Vec<DstackEventLog> =
            serde_json::from_value(serde_json::to_value(&upstream).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&port).unwrap(),
            serde_json::to_value(&upstream).unwrap()
        );
        assert!(validate_v2_preimages(&port).is_ok());
        for (ours, theirs) in port.iter().zip(&runtime) {
            assert_eq!(ours.runtime_preimage().unwrap(), theirs.preimage());
            let digest: [u8; 48] = Sha384::digest(ours.runtime_preimage().unwrap()).into();
            assert_eq!(digest, theirs.sha384_digest());
        }
        assert_eq!(
            replay_rtmr3(&port).unwrap(),
            cc_eventlog::replay_events::<ez_hash::Sha384>(&runtime, None)
        );
    }
}
