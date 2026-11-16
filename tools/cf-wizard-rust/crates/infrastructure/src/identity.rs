//! CompatibilityIdentityV1 preserves the published transfer-subsystem identity.
//! Source and GeneratedType UUIDs are never generated through this function.
use std::fmt::Write;

pub(crate) fn transfer_subsystem(configuration_uuid: &str) -> String {
    let seed = format!("{configuration_uuid}/partial-cf/_Перенос");
    let state = seed.as_bytes().iter().fold(
        [
            1_469_598_103_934_665_603_u64,
            1_099_511_628_211_u64 ^ 0x9e3779b97f4a7c15,
        ],
        |[left, right], &byte| {
            [
                (left ^ u64::from(byte)).wrapping_mul(1_099_511_628_211),
                (right ^ u64::from(byte))
                    .wrapping_mul(1_099_511_628_211)
                    .wrapping_add(0x7f4a7c15),
            ]
        },
    );
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&state[0].to_be_bytes());
    bytes[8..].copy_from_slice(&state[1].to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut uuid = String::with_capacity(36);
    for (position, byte) in bytes.iter().enumerate() {
        if matches!(position, 4 | 6 | 8 | 10) {
            uuid.push('-');
        }
        write!(uuid, "{byte:02x}").expect("writing a String cannot fail");
    }
    uuid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_identity_vectors_and_utf8_seed_are_unchanged() {
        for (source_uuid, expected) in [
            ("", "0b3dbda4-f860-4308-8509-ad76cdcd288e"),
            (
                "00000000-0000-0000-0000-000000000000",
                "7b2a800d-3c48-4a4c-b346-c1098731b04e",
            ),
            (
                "12345678-1234-4234-9234-123456789abc",
                "e02c6b4b-2d3d-4e58-bd8c-90b2485b6ece",
            ),
        ] {
            assert_eq!(transfer_subsystem(source_uuid), expected);
            assert_eq!(
                transfer_subsystem(source_uuid),
                crate::metadata::stable_uuid(&format!("{source_uuid}/partial-cf/_Перенос"))
            );
        }
    }
}
