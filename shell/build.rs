// Embeds, in defenestrate-shell-uiaccess.exe only, the manifest that asks
// Windows for uiAccess: a medium integrity process is cut off from elevated
// windows by UIPI (no input through its low-level hooks while one has focus,
// no moving them), and uiAccess is the documented way past that for a
// process that isn't elevated itself:
// https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-only-elevate-uiaccess-applications-that-are-installed-in-secure-locations
//
// Windows grants it only to an exe signed by a certificate its Trusted Root
// store has, under Program Files (sign.sh, cli/src/cert.rs). asInvoker:
// uiAccess is all it gets, never elevation. Nothing else goes in the
// manifest, so the two binaries differ in nothing else (DPI awareness, for
// one, is still set at runtime). Why there are two: src/handoff.rs.
use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, empty_manifest};

const UIACCESS_BIN: &str = "defenestrate-shell-uiaccess";
const EMIT: &str = "DEFENESTRATE_EMBED_MANIFEST";

fn main() {
    if std::env::var_os(EMIT).is_some() {
        let manifest = empty_manifest().requested_execution_level(ExecutionLevel::AsInvoker).ui_access(true);
        embed_manifest(manifest).expect("embedding the manifest");
        return;
    }
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    // embed-manifest links its object into every binary of the package
    // (rustc-link-arg-bins, and nothing narrower), which would make the
    // launcher ask for uiAccess too:
    // https://codeberg.org/carey/embed-manifest/src/tag/v1.5.1/src/embed/mod.rs#L111
    // So this script runs itself to have it write the object, keeps that
    // instruction from Cargo, and links the object into one binary. Remove
    // once embed-manifest can target a single binary.
    let run = std::process::Command::new(std::env::current_exe().unwrap()).env(EMIT, "1").output().unwrap();
    assert!(run.status.success(), "embedding the manifest: {}", String::from_utf8_lossy(&run.stderr));
    let stdout = String::from_utf8(run.stdout).unwrap();
    let objects: Vec<&str> = stdout.lines().filter_map(|l| l.strip_prefix("cargo:rustc-link-arg-bins=")).collect();
    let [object] = objects[..] else { panic!("embed-manifest said {stdout:?}, not one object to link") };
    println!("cargo::rustc-link-arg-bin={UIACCESS_BIN}={object}");
}
