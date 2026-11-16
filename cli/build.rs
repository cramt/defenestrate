// Compiles the Windows shell into dfnstrate when DEFENESTRATE_SHELL_DIR
// points at shell/sign.sh's output, so `shell = "builtin"` works from a
// single binary. The Nix package always sets it; a plain `cargo build`
// doesn't have a Windows cross toolchain to make one, so it's optional,
// unless the builtin-shell feature (on in release builds) asks for it.
//
// A directory, not one exe: the shell is a launcher plus the signed uiAccess
// binary it starts (shell/src/handoff.rs), and they only work as a pair.
const EXES: [&str; 2] = ["defenestrate-shell.exe", "defenestrate-shell-uiaccess.exe"];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(dfnstrate_builtin_shell)");
    println!("cargo::rerun-if-env-changed=DEFENESTRATE_SHELL_DIR");
    // The variable this replaced held the launcher alone; say so rather than
    // quietly building without the shell.
    println!("cargo::rerun-if-env-changed=DEFENESTRATE_SHELL_EXE");
    if std::env::var_os("DEFENESTRATE_SHELL_EXE").is_some() {
        panic!("DEFENESTRATE_SHELL_EXE is gone: set DEFENESTRATE_SHELL_DIR to shell/sign.sh's output directory (README)");
    }
    if let Some(dir) = std::env::var_os("DEFENESTRATE_SHELL_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        for exe in EXES {
            let (from, to) = (dir.join(exe), out.join(exe));
            // Store files are read-only and copy keeps that, so a rebuild
            // can't overwrite the last copy.
            let _ = std::fs::remove_file(&to);
            std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("DEFENESTRATE_SHELL_DIR: {}: {e}", from.display()));
            println!("cargo::rerun-if-changed={}", from.display());
        }
        println!("cargo::rustc-cfg=dfnstrate_builtin_shell");
    } else if std::env::var_os("CARGO_FEATURE_BUILTIN_SHELL").is_some() {
        panic!("the builtin-shell feature needs DEFENESTRATE_SHELL_DIR: shell/sign.sh's output directory (README)");
    }
}
