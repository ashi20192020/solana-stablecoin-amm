use anchor_lang::{AnchorDeserialize, Discriminator};

/// Anchor writes events as base64 `Program data:` log lines. Decoding them inline keeps
/// the test crate free of a base64 dependency.
pub fn decode_events<E: Discriminator + AnchorDeserialize>(logs: &[String]) -> Vec<E> {
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(decode_base64)
        .filter_map(|bytes| {
            let discriminator = E::DISCRIMINATOR;
            if bytes.len() < discriminator.len() || &bytes[..discriminator.len()] != discriminator {
                return None;
            }
            E::deserialize(&mut &bytes[discriminator.len()..]).ok()
        })
        .collect()
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(bytes)
}
