fn main() {
    tauri_build::build();

    // Tauri embeds the Windows resource (icon, version info, and the
    // application manifest) with `cargo:rustc-link-arg-bins`, which applies
    // to bins only. A test binary therefore links without the manifest, and
    // the manifest is what requests Common-Controls v6. Without it the loader
    // binds comctl32 v5 from System32, an export the webview layer needs is
    // missing, and the process dies before `main` with
    // STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) and no message.
    //
    // Linking the same resource into test binaries lets integration tests
    // that touch the Tauri runtime start on Windows.
    #[cfg(windows)]
    {
        let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
        let resource = std::path::Path::new(&out).join("resource.lib");
        if resource.exists() {
            println!("cargo:rustc-link-arg-tests={}", resource.display());
        } else {
            // Do not fail the build: without the resource the tests cannot
            // start, but the app still builds and ships. Warn explicitly so
            // the failure is not left as a bare NTSTATUS.
            println!(
                "cargo:warning=no resource.lib in OUT_DIR; Tauri test binaries \
                 will fail to start on Windows with STATUS_ENTRYPOINT_NOT_FOUND"
            );
        }
    }
}
