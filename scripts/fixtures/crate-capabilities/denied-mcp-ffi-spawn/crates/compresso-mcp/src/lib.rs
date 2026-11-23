#![forbid(unsafe_code)]
unsafe extern "C" { fn posix_spawnp(pid: *mut i32) -> i32; }
