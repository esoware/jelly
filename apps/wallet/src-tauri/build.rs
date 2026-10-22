#[cfg(feature = "test-util")]
use std::env;
use std::error::Error;
#[cfg(feature = "test-util")]
use std::path::PathBuf;

macro_rules! jelly_commands {
    ( $( $area:ident : [ $( $name:ident ),+ $(,)? ] ),+ $(,)? ) => {
        const COMMAND_NAMES: &[&str] = &[ $( $( stringify!($name) ),+ ),+ ];
    };
}

include!("commands.rs");

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=commands.rs");
    let manifest = tauri_build::AppManifest::new().commands(COMMAND_NAMES);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))?;

    // Tauri links its common-controls-v6 manifest into the app binary only.
    // Integration test executables also link Wry and need the same manifest.
    #[cfg(feature = "test-util")]
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let resource = PathBuf::from(env::var("OUT_DIR")?).join("resource.lib");
        println!("cargo::rustc-link-arg-tests={}", resource.display());
    }
    Ok(())
}
