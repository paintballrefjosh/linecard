#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|xml: &str| {
    let _ = linecard_importers::mremoteng::parse(xml);
});
