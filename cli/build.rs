// Compiles the Windows shell into dfnstrate when DEFENESTRATE_SHELL_EXE
// points at one, so `shell = "builtin"` works from a single binary. The Nix
// package always sets it; a plain `cargo build` doesn't have a Windows
// cross toolchain to make one, so it's optional.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(dfnstrate_builtin_shell)");
    println!("cargo::rerun-if-env-changed=DEFENESTRATE_SHELL_EXE");
    if let Some(exe) = std::env::var_os("DEFENESTRATE_SHELL_EXE") {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("defenestrate-shell.exe");
        // Store files are read-only and copy keeps that, so a rebuild can't
        // overwrite the last copy.
        let _ = std::fs::remove_file(&out);
        std::fs::copy(&exe, &out).unwrap_or_else(|e| panic!("DEFENESTRATE_SHELL_EXE={}: {e}", exe.to_string_lossy()));
        println!("cargo::rerun-if-changed={}", exe.to_string_lossy());
        println!("cargo::rustc-cfg=dfnstrate_builtin_shell");
    }
}
