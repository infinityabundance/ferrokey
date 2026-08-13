//! Fuzz target for the Ferrokey protocol decoder.
//!
//! The protocol is a privilege boundary (ferrokeyd's hostile-input surface):
//! no malformed client data may panic, over-allocate, or corrupt daemon
//! state. This target feeds libFuzzer's mutations into the streaming
//! [`Decoder`] using fragmented delivery, so both the framing logic and the
//! payload parsers are exercised on arbitrary input.
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run fuzz_decoder -- -max_total_time=120 -rss_limit_mb=2048
//! ```
//!
//! The same property is continuously verified on stable by
//! `codec::tests::hostile_input_never_panics_and_stays_bounded`.

#![no_main]

use ferrokey_protocol::Decoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // Fragment the input: chunk sizes are derived from the input itself so
    // the streaming "partial frame" state machine is exercised, not just
    // whole-frame parsing. The decoder must never panic.
    let mut decoder = Decoder::new();
    let mut i = 0;
    while i < data.len() {
        let chunk = (1 + (usize::from(data[i]) % 31)).min(data.len() - i);
        let _ = decoder.push(&data[i..i + chunk]);
        i += chunk;
    }
    // Deliver the trailing partial frame (if any) one byte at a time.
    let _ = decoder.push(&[]);
});
