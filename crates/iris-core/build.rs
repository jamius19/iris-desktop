/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Compiles Sonic (`vendor/sonic`, Apache-2.0) for `audio::time_stretch`, as Handy's `build.rs` does.

fn main() {
    println!("cargo:rerun-if-changed=../../vendor/sonic");
    cc::Build::new().file("../../vendor/sonic/sonic.c").include("../../vendor/sonic").warnings(false).compile("sonic");
}
