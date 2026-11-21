use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/cf-wizard.rc");
    println!("cargo:rerun-if-changed=assets/cf-wizard.ico");

    let target = env::var("TARGET").expect("Cargo target");
    if !target.ends_with("-windows-msvc") {
        return;
    }

    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture");
    let sdk = find_msvc_tools::find_windows_sdk(&arch).expect("Windows SDK for the CF Wizard icon");
    let compiler = sdk
        .path()
        .map(|path| path.join("rc.exe"))
        .find(|path| path.is_file())
        .expect("Windows SDK resource compiler rc.exe");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(
        out.join("cf-wizard-version.h"),
        format!(
            "#define CF_WIZARD_VERSION_NUM {},{},{},0\n#define CF_WIZARD_VERSION_TEXT \"{}\\0\"\n",
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            env!("CARGO_PKG_VERSION_PATCH"),
            env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("write CF Wizard version resource definitions");
    let resource = out.join("cf-wizard.res");
    let status = Command::new(compiler)
        .current_dir("assets")
        .args(["/nologo", "/i"])
        .arg(&out)
        .arg("/fo")
        .arg(&resource)
        .arg("cf-wizard.rc")
        .status()
        .expect("launch Windows resource compiler");
    assert!(status.success(), "compile CF Wizard resources");
    println!(
        "cargo:rustc-link-arg-bin=cf-wizard-rust={}",
        resource.display()
    );
}
