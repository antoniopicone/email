//! Compile the icon set into a GResource bundle.
//!
//! `glib-compile-resources` ships with GLib's development files, which are
//! already required to build against GTK, so this adds no new dependency.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ICON_DIR: &str = "data/icons";
const MANIFEST: &str = "data/mailview.gresource.xml";

/// `cargo:rerun-if-changed` needs every source file named, not just the
/// top-level directory, or edits to a nested icon go unnoticed.
fn watch_recursively(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            watch_recursively(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed={MANIFEST}");
    println!("cargo:rerun-if-changed=build.rs");
    watch_recursively(Path::new(ICON_DIR));

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
