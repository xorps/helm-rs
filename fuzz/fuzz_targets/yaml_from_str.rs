//! `yaml::from_str` on arbitrary input must not panic; whatever it accepts
//! must also serialize without panicking (`toYaml` / `toYamlPretty`).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(v) = helm_rs::yaml::from_str(text) {
        let _ = helm_rs::yaml::to_string(&v);
        let _ = helm_rs::yaml::to_string_pretty(&v);
    }
});
