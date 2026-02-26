//! Build script for the Flatline crate.
//!
//! Exposes the `TARGET` environment variable at compile time for
//! constructing platform-specific download URLs in the updater.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=TARGET={target}");
}
