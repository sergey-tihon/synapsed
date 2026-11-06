use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=macos/Info.plist");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
                .join("macos/Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=synapsed=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_WEB_UI");
    if env::var_os("CARGO_FEATURE_WEB_UI").is_none() {
        return;
    }
    let directory =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
            .join("../../apps/web/dist/client");
    println!("cargo:rerun-if-changed={}", directory.display());
    let index = directory.join("index.html");
    assert!(
        index.is_file(),
        "The embedded dashboard is missing. Run `npm run build:web` before building synapsed with web-ui."
    );
    let directory = directory
        .canonicalize()
        .expect("Cannot resolve built dashboard directory; run `npm run build:web`.");
    println!("cargo:rustc-env=SYNAPSED_WEB_DIR={}", directory.display());
}
