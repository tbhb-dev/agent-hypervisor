//! Builds `libghostty-vt.a` from the pinned Ghostty commit and links it statically.
//!
//! The source comes from `GHOSTTY_VT_SOURCE`, a local Ghostty clone that holds the pinned commit,
//! or else from a shallow fetch of that commit from GitHub. Git checks the commit hash either way.
//! Zig must be on `PATH` (mise installs the pinned version) or named by `ZIG`. The archive is
//! copied into a directory of its own before linking, because with the sibling dylib in the same
//! directory the Apple linker links the dylib and the binary then fails to load (RFC-36 run 2).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GHOSTTY_URL: &str = "https://github.com/ghostty-org/ghostty.git";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ghostty.pin");
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_SOURCE");
    println!("cargo:rerun-if-env-changed=ZIG");

    let pin = fs::read_to_string("ghostty.pin").expect("read ghostty.pin");
    let pin = pin.trim();
    assert!(
        pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit()),
        "ghostty.pin must hold a full 40-character commit hash, found {pin:?}"
    );

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let link_dir = out.join("lib-static");
    let archive = link_dir.join("libghostty-vt.a");
    let stamp = out.join("ghostty.commit");
    let built = fs::read_to_string(&stamp).is_ok_and(|s| s == pin) && archive.is_file();
    if !built {
        build(pin, &out, &link_dir);
        fs::write(&stamp, pin).expect("write the build stamp");
    }

    println!("cargo:rustc-link-search=native={}", link_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");
}

fn build(pin: &str, out: &Path, link_dir: &Path) {
    let src = out.join("ghostty-src");
    let prefix = out.join("zig-out");
    let cache = out.join("zig-cache");
    for dir in [&src, &prefix, &cache, &link_dir.to_path_buf()] {
        if dir.exists() {
            fs::remove_dir_all(dir).expect("clear a stale build directory");
        }
    }
    fs::create_dir_all(&src).expect("create the source directory");
    fetch(pin, &src);

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let jobs = env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());
    let mut cmd = Command::new(&zig);
    cmd.current_dir(&src)
        .arg("build")
        .arg(format!("-j{jobs}"))
        .args([
            "-Demit-lib-vt",
            "-Doptimize=ReleaseFast",
            "-Demit-xcframework=false",
        ])
        .arg("--prefix")
        .arg(&prefix)
        .arg("--cache-dir")
        .arg(&cache);
    if let Some(target) = zig_target() {
        cmd.arg(format!("-Dtarget={target}"));
    }
    // Baseline CPU features, so an archive cached on one CI machine runs on another.
    cmd.arg("-Dcpu=baseline");
    run(
        &mut cmd,
        "zig build -Demit-lib-vt (Zig 0.16.0 from mise, or set ZIG)",
    );

    fs::create_dir_all(link_dir).expect("create the link directory");
    fs::copy(
        prefix.join("lib").join("libghostty-vt.a"),
        link_dir.join("libghostty-vt.a"),
    )
    .expect("copy libghostty-vt.a");
    // The source tree and Zig's local cache run to about a gigabyte; only the archive is needed.
    for dir in [&src, &cache] {
        fs::remove_dir_all(dir).expect("remove a build directory");
    }
}

fn fetch(pin: &str, src: &Path) {
    if let Some(local) = env::var_os("GHOSTTY_VT_SOURCE") {
        let mut archive = Command::new("git")
            .arg("-C")
            .arg(&local)
            .args(["archive", "--format=tar", pin])
            .stdout(Stdio::piped())
            .spawn()
            .expect("run git archive in GHOSTTY_VT_SOURCE");
        let tar_in = archive.stdout.take().expect("git archive stdout");
        run(
            Command::new("tar")
                .arg("-x")
                .arg("-C")
                .arg(src)
                .stdin(tar_in),
            "tar -x of the Ghostty archive",
        );
        assert!(
            archive.wait().expect("wait for git archive").success(),
            "git archive {pin} failed in GHOSTTY_VT_SOURCE; does the clone hold the pinned commit?"
        );
    } else {
        run(
            Command::new("git").arg("-C").arg(src).args(["init", "-q"]),
            "git init",
        );
        run(
            Command::new("git").arg("-C").arg(src).args([
                "fetch",
                "-q",
                "--depth",
                "1",
                GHOSTTY_URL,
                pin,
            ]),
            "git fetch of the pinned Ghostty commit",
        );
        run(
            Command::new("git")
                .arg("-C")
                .arg(src)
                .args(["checkout", "-q", pin]),
            "git checkout of the pinned Ghostty commit",
        );
    }
}

/// Zig's name for the cargo target, or `None` to let Zig build for the host.
fn zig_target() -> Option<&'static str> {
    let target = env::var("TARGET").expect("TARGET");
    let host = env::var("HOST").expect("HOST");
    let zig = match target.as_str() {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "aarch64-apple-darwin" if target != host => "aarch64-macos",
        "x86_64-apple-darwin" if target != host => "x86_64-macos",
        "aarch64-apple-darwin" | "x86_64-apple-darwin" => return None,
        other => panic!("no Zig target known for {other}"),
    };
    Some(zig)
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|err| panic!("{what}: could not start: {err}"));
    assert!(status.success(), "{what}: {status}");
}
