#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = linecard_importers::securecrt::parse_session(Vec::new(), "fuzz", text);
    let _ = linecard_importers::securecrt_keywords::parse(text, "fuzz");
});
