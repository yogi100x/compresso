#![deny(unsafe_code)]
unsafe extern "C" { fn system(command: *const std::ffi::c_char) -> i32; }
