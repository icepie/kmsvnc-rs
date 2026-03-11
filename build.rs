fn main() {
    let mut build = cc::Build::new();
    build.file("src/kms/vaapi_shim.c").flag_if_supported("-std=c11");

    if let Ok(output) = std::process::Command::new("pkg-config")
        .args(["--cflags", "libdrm"])
        .output()
    {
        if output.status.success() {
            if let Ok(cflags) = String::from_utf8(output.stdout) {
                for flag in cflags.split_whitespace() {
                    if let Some(include) = flag.strip_prefix("-I") {
                        build.include(include);
                    }
                }
            }
        }
    }

    build.compile("kmsvnc_vaapi");

    println!("cargo:rerun-if-changed=src/kms/vaapi_shim.c");
    println!("cargo:rustc-link-lib=va");
    println!("cargo:rustc-link-lib=va-drm");
    println!("cargo:rustc-link-lib=drm");
}
