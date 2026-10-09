//! `include_dir!` does not tell cargo which files it read, so a rule edited
//! under `rules/` would otherwise leave a stale build running the old rule.
//!
//! The script also fetches the pinned pdfium library for the target platform
//! and hands it to `src/pdfium.rs`, which embeds it in the binary.
//!
//! The release is pinned: pdfium's antialiasing changes between builds, and the
//! ABI must match the `pdfium_XXXX` feature in
//! `vendor/crates/u2s-render-pdf/Cargo.toml`, which is upstream's to change.
//! Bumping it means pinning a u2s revision with the new feature, then updating
//! `RELEASE` and every checksum in `ASSETS`.
//!
//! The download is cached in `vendor/pdfium/` at the workspace root. A
//! `RELEASE` marker there records what was unpacked, so a matching cache never
//! touches the network.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const RELEASE: &str = "chromium/7881";

/// `(target_os, target_arch, asset, sha256 of the asset)`.
const ASSETS: &[(&str, &str, &str, &str)] = &[
    ("macos", "aarch64", "pdfium-mac-arm64.tgz", "52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40"),
    ("macos", "x86_64", "pdfium-mac-x64.tgz", "6dedf83990e0e3d6b7c93c9e7589c5a126b0ae14b7464d76120cff7a26afb18b"),
    ("linux", "x86_64", "pdfium-linux-x64.tgz", "1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d"),
    ("linux", "aarch64", "pdfium-linux-arm64.tgz", "ee7f7b7d5468958336a818c1cd580bdd20972846b7377b13f9a923d92d1d4674"),
    ("windows", "x86_64", "pdfium-win-x64.tgz", "73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08"),
];

fn main() {
    println!("cargo:rerun-if-changed=../rules/aem");
    println!("cargo:rerun-if-changed=build.rs");
    if let Err(e) = run() {
        panic!("cannot provide pdfium: {e}");
    }
}

fn run() -> Result<(), String> {
    let os = env("CARGO_CFG_TARGET_OS")?;
    let arch = env("CARGO_CFG_TARGET_ARCH")?;
    let &(_, _, asset, sha256) = ASSETS
        .iter()
        .find(|(o, a, _, _)| *o == os && *a == arch)
        .ok_or_else(|| format!("no pinned pdfium build for {os}/{arch}"))?;
    let library = match os.as_str() {
        "macos" => "libpdfium.dylib",
        "windows" => "pdfium.dll",
        _ => "libpdfium.so",
    };

    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR")?);
    let vendor = manifest
        .parent()
        .ok_or("the agent crate has no parent directory")?
        .join("vendor/pdfium");
    let marker = format!("{RELEASE} {asset} {sha256}\n");
    let cached = fs::read_to_string(vendor.join("RELEASE")).is_ok_and(|m| m == marker)
        && vendor.join("lib").join(library).is_file();
    if !cached {
        fetch(&vendor, asset, sha256, &marker)?;
    }
    println!("cargo:rerun-if-changed={}", vendor.join("RELEASE").display());

    let bytes = fs::read(vendor.join("lib").join(library)).map_err(|e| format!("{library}: {e}"))?;
    let out = PathBuf::from(env("OUT_DIR")?);
    fs::write(out.join("pdfium.lib"), &bytes).map_err(|e| e.to_string())?;
    println!("cargo:rustc-env=PDFIUM_LIBRARY_NAME={library}");
    println!("cargo:rustc-env=PDFIUM_LIBRARY_SHA256={}", hex(&Sha256::digest(&bytes)));
    Ok(())
}

/// Downloads and checks the asset, unpacks it beside `vendor`, and moves it
/// into place only once it is complete, so an interrupted build leaves no
/// half-written cache behind.
fn fetch(vendor: &Path, asset: &str, sha256: &str, marker: &str) -> Result<(), String> {
    let url = format!("https://github.com/bblanchon/pdfium-binaries/releases/download/{RELEASE}/{asset}");
    println!("cargo:warning=fetching {url}");
    let mut archive = Vec::new();
    // The whole body counts against the client's timeout, which by default
    // would fail a first build on a slow link.
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .and_then(|client| client.get(&url).send())
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("downloading {url} failed ({e}); a first build needs network access"))?
        .read_to_end(&mut archive)
        .map_err(|e| format!("downloading {url} failed: {e}"))?;
    let actual = hex(&Sha256::digest(&archive));
    if actual != sha256 {
        return Err(format!("checksum mismatch for {asset}: expected {sha256}, got {actual}"));
    }

    let staging = vendor.with_file_name(format!("pdfium.partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    tar::Archive::new(flate2::read::GzDecoder::new(archive.as_slice()))
        .unpack(&staging)
        .map_err(|e| format!("unpacking {asset}: {e}"))?;
    // The Windows archive ships the DLL in bin/, but the loader only looks in lib/.
    let dll = staging.join("bin/pdfium.dll");
    if dll.is_file() {
        fs::create_dir_all(staging.join("lib")).map_err(|e| e.to_string())?;
        fs::rename(&dll, staging.join("lib/pdfium.dll")).map_err(|e| e.to_string())?;
    }
    fs::write(staging.join("RELEASE"), marker).map_err(|e| e.to_string())?;

    let _ = fs::remove_dir_all(vendor);
    if let Err(e) = fs::rename(&staging, vendor) {
        let _ = fs::remove_dir_all(&staging);
        // Another build script (a second target dir on this checkout) may
        // have put the same release in place first.
        if fs::read_to_string(vendor.join("RELEASE")).is_ok_and(|m| m == marker) {
            return Ok(());
        }
        return Err(format!("moving pdfium into {}: {e}", vendor.display()));
    }
    Ok(())
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
