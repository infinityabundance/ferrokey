//! Fuzz target for the Ferrokey layout YAML parser.
//!
//! `parse_layout` consumes user- and court-supplied layout files — data that
//! crosses the same trust boundary as the protocol decoder (a hostile layout
//! must never panic the layout loader, and — for layouts that do parse — the
//! xkb validation gate must never panic either). This target feeds
//! libFuzzer's mutations into the parser as lossy UTF-8 (YAML is
//! byte-oriented, so arbitrary bytes are legal input).
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run fuzz_parser -- -max_total_time=120 -rss_limit_mb=2048 -max_len=65536
//! ```
//!
//! The same property is continuously verified on stable by
//! `builtin::tests::hostile_yaml_never_panics_and_stays_bounded`.

#![no_main]

use ferrokey_layouts::{parse_layout, validate_layout};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // Arbitrary bytes are legal YAML; the parser must never panic.
    let text = String::from_utf8_lossy(data);
    if let Ok(layout) = parse_layout(&text) {
        // A layout that parses must also survive the full xkb capability
        // validation without panicking (built-in loading runs both).
        let _ = validate_layout(&layout);
    }
});
