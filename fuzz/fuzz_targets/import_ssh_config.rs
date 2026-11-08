#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = linecard_importers::ssh_config::parse(text);
});
