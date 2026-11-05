#![no_main]

use libfuzzer_sys::fuzz_target;

// Anything another device (or the relay) can hand us: a sealed payload as JSON
// must be rejected or opened, never crash.
fuzz_target!(|data: &[u8]| {
    if let Ok(sealed) = serde_json::from_slice::<linecard_sync::Sealed>(data) {
        let group = linecard_sync::GroupKey::from_bytes([7; 32]);
        let _ = linecard_sync::crypto::open::<serde_json::Value>(&group, &sealed);
    }
});
