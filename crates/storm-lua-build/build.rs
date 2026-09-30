//! Capture producer source revision for optimization explanation artifacts.
use std::{env, path::PathBuf, process::Command};

fn git(root: &PathBuf, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing cargo manifest path")?)
            .join("../..");
    println!("cargo:rerun-if-env-changed=STORM_LUA_SOURCE_REVISION");
    println!("cargo:rerun-if-env-changed=STORM_LUA_SOURCE_DIRTY");
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "crates/storm-lua-syntax",
        "crates/storm-lua-minify",
        "crates/storm-lua-build",
        "crates/storm-lua-analysis",
        "crates/storm-lua-spec",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    for path in ["HEAD", "index"] {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
    }
    if let Some(reference) = git(&root, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
    }
    let revision = env::var("STORM_LUA_SOURCE_REVISION")
        .ok()
        .or_else(|| git(&root, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    assert!(
        revision == "unknown"
            || (revision.len() >= 7
                && revision.len() <= 64
                && revision.bytes().all(|b| b.is_ascii_hexdigit())),
        "invalid source revision"
    );
    let dirty = env::var("STORM_LUA_SOURCE_DIRTY")
        .ok()
        .or_else(|| {
            git(
                &root,
                &["status", "--porcelain", "--untracked-files=normal"],
            )
            .map(|s| (!s.is_empty()).to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    assert!(
        ["true", "false", "unknown"].contains(&dirty.as_str()),
        "invalid source dirty flag"
    );
    println!("cargo:rustc-env=STORM_LUA_SOURCE_REVISION={revision}");
    println!("cargo:rustc-env=STORM_LUA_SOURCE_DIRTY={dirty}");
    Ok(())
}
