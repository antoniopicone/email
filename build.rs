//! Compile the icon set into a GResource bundle.
//!
//! `glib-compile-resources` ships with GLib's development files, which are
//! already required to build against GTK, so this adds no new dependency.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ICON_DIR: &str = "data/icons/scalable/actions";
const MANIFEST: &str = "data/mailview.gresource.xml";

fn main() {
    println!("cargo:rerun-if-changed={MANIFEST}");
    println!("cargo:rerun-if-changed=build.rs");
    if let Ok(entries) = fs::read_dir(ICON_DIR) {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is always set by cargo"));
    let target = out_dir.join("mailview.gresource");

    let output = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(ICON_DIR)
        .arg("--target")
        .arg(&target)
        .arg(MANIFEST)
        .output();

    match output {
        Ok(result) if result.status.success() => {}
        Ok(result) => panic!(
            "glib-compile-resources failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        ),
        Err(e) => panic!(
            "could not run glib-compile-resources ({e}).\n\
             Install GLib's development tools: `apt install libglib2.0-dev` or \
             `dnf install glib2-devel`."
        ),
    }

    assert!(Path::new(&target).exists(), "the resource bundle was not produced");
}
