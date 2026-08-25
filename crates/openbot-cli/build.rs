//! Rebuild when the shipped credential changes.
//!
//! `config::builtin_key` reads `OPENBOT_BUILTIN_KEY` with `option_env!`, which
//! is resolved when this crate is compiled. Cargo does not watch environment
//! variables by default, so without this line a release built after rotating
//! the key would quietly reuse the cached object file and ship the old one —
//! the failure being invisible until somebody's install stops working.
fn main() {
    println!("cargo:rerun-if-env-changed=OPENBOT_BUILTIN_KEY");
}
