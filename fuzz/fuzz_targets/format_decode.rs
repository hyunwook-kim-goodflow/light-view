#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| lv_core::fuzzing::format_decode(data));
