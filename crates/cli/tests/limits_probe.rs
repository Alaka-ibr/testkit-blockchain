//! Integration tests for how `soroban-testkit limits` starts its probe
//! subprocesses: the binary must work when invoked through `PATH` or a
//! symlink (issue #188), and a run must leave no temporary files or
//! snapshot artifacts behind (issue #189).
//!
//! Every test runs the real binary against a real `vault.wasm`, built once
//! into Cargo's per-target integration-test scratch directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const BIN: &str = env!("CARGO_BIN_EXE_soroban-testkit");
const FUNCTION: &str = "batch_payout";
const RAMP: &str = "recipients";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Builds `examples/vault` to wasm once per test binary and returns its path.
fn vault_wasm() -> &'static Path {
    static WASM: OnceLock<PathBuf> = OnceLock::new();
    WASM.get_or_init(|| {
        let target_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("limits-probe-target");
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "vault",
                "--target",
                "wasm32v1-none",
                "--release",
                "--target-dir",
            ])
            .arg(&target_dir)
            .current_dir(workspace_root())
            .status()
            .expect("failed to build vault.wasm");
        assert!(status.success(), "cargo build for vault must succeed");
        let wasm = target_dir.join("wasm32v1-none/release/vault.wasm");
        assert!(wasm.is_file(), "expected vault.wasm at {wasm:?}");
        wasm
    })
}

/// A uniquely named temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("limits-probe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn sub(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).expect("create sub dir");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn limits_args(contract: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "limits".into(),
        "--contract".into(),
        contract.into(),
        "--fn".into(),
        FUNCTION.into(),
        "--ramp".into(),
        RAMP.into(),
    ]
}

fn assert_found_a_limit(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("last successful recipients"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("instructions:"), "{stdout}");
}

/// Names of everything in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Places the real binary in `dir` under its usual name: a symlink where
/// the platform supports one without privileges, a copy on Windows.
fn install_binary(dir: &Path) -> PathBuf {
    let dest = dir.join(format!("soroban-testkit{}", std::env::consts::EXE_SUFFIX));
    #[cfg(unix)]
    std::os::unix::fs::symlink(BIN, &dest).expect("symlink binary");
    #[cfg(not(unix))]
    std::fs::copy(BIN, &dest).expect("copy binary");
    dest
}

#[cfg(unix)]
#[test]
fn limits_probes_start_when_the_binary_is_invoked_through_a_symlink() {
    let tmp = TempDir::new("symlink");
    let link = install_binary(&tmp.sub("bin"));
    assert!(std::fs::symlink_metadata(&link)
        .expect("link metadata")
        .file_type()
        .is_symlink());

    let output = Command::new(&link)
        .args(limits_args(vault_wasm()))
        .current_dir(tmp.sub("cwd"))
        .output()
        .expect("run soroban-testkit through a symlink");

    assert_found_a_limit(&output);
}

#[test]
fn limits_probes_start_when_the_binary_is_found_through_path() {
    let tmp = TempDir::new("path");
    let bin_dir = tmp.sub("bin");
    install_binary(&bin_dir);
    let mut path = vec![bin_dir];
    if let Some(existing) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&existing));
    }

    let output = Command::new("soroban-testkit")
        .args(limits_args(vault_wasm()))
        .env("PATH", std::env::join_paths(path).expect("join PATH"))
        .current_dir(tmp.sub("cwd"))
        .output()
        .expect("run soroban-testkit through PATH");

    assert_found_a_limit(&output);
}

#[test]
fn limits_leaves_no_temporary_files_or_snapshot_artifacts() {
    let tmp = TempDir::new("cleanup");
    let project = tmp.sub("project");
    let temp = tmp.sub("tmp");
    // A relative --contract also checks that probes, which run in their
    // own scratch directory, still find the contract.
    std::fs::copy(vault_wasm(), project.join("contract.wasm")).expect("copy wasm");

    let output = Command::new(BIN)
        .args(limits_args(Path::new("contract.wasm")))
        .current_dir(&project)
        .env("TMPDIR", &temp)
        .env("TMP", &temp)
        .env("TEMP", &temp)
        .output()
        .expect("run soroban-testkit");

    assert_found_a_limit(&output);
    assert_eq!(
        entries(&project),
        ["contract.wasm"],
        "limits must not write snapshots or other files into the working directory"
    );
    assert!(
        entries(&temp).is_empty(),
        "limits must remove its probe scratch directory: {:?}",
        entries(&temp)
    );
}

#[test]
fn limits_cleans_up_its_scratch_directory_when_every_probe_fails() {
    let tmp = TempDir::new("cleanup-on-error");
    let project = tmp.sub("project");
    let temp = tmp.sub("tmp");

    // `withdraw` panics without an initialized owner, so every probe fails
    // and `limits` exits with an error after spawning probes.
    let output = Command::new(BIN)
        .args([
            "limits".as_ref(),
            "--contract".as_ref(),
            vault_wasm().as_os_str(),
            "--fn".as_ref(),
            "withdraw".as_ref(),
            "--ramp".as_ref(),
            "amount".as_ref(),
        ])
        .current_dir(&project)
        .env("TMPDIR", &temp)
        .env("TMP", &temp)
        .env("TEMP", &temp)
        .output()
        .expect("run soroban-testkit");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("failed even at the smallest ramp value"),
        "{stderr}"
    );
    assert!(entries(&project).is_empty(), "{:?}", entries(&project));
    assert!(entries(&temp).is_empty(), "{:?}", entries(&temp));
}
