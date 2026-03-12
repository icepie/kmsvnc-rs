fn main() {
    compile_c_helper("kmsvnc_vaapi", &["src/kms/vaapi_shim.c"], &["libdrm"]);
    compile_c_helper("kmsvnc_x264", &["src/encode/x264_shim.c"], &["x264"]);

    println!("cargo:rerun-if-changed=src/kms/vaapi_shim.c");
    println!("cargo:rerun-if-changed=src/encode/x264_shim.c");
    println!("cargo:rustc-link-lib=va");
    println!("cargo:rustc-link-lib=va-drm");
    println!("cargo:rustc-link-lib=drm");
    println!("cargo:rustc-link-lib=x264");
}

fn compile_c_helper(lib_name: &str, files: &[&str], pkg_names: &[&str]) {
    let mut build = cc::Build::new();
    build.flag_if_supported("-std=c11");
    for file in files {
        build.file(file);
    }

    for pkg_name in pkg_names {
        if let Ok(output) = std::process::Command::new("pkg-config")
            .args(["--cflags", pkg_name])
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
    }

    build.compile(lib_name);
}
