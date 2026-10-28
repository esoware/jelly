use std::env;
use std::error::Error;
use std::path::PathBuf;

// proto/core/** is java-tron's protocol/src/main/protos at commit
// f8b05d40abc949fa588ab64d8fd8fd82845ebeed (GreatVoyage-v4.8.2), fetched 2026-08-13, copied
// verbatim except for one non-ASCII character in a Tron.proto comment. java-tron is
// LGPL-3.0, whose text is proto/COPYING.LESSER, and proto/COPYING is the GPL-3.0 it builds on.
//
// Tron.proto holds Transaction and imports core/Discover.proto, core/contract/common.proto,
// and google/protobuf/any.proto, which protox supplies. The two contract files add
// TransferContract and TriggerSmartContract, the only contract bodies we build.
const ROOTS: [&str; 3] = [
    "core/Tron.proto",
    "core/contract/balance_contract.proto",
    "core/contract/smart_contract.proto",
];
const IMPORTS: [&str; 2] = ["core/Discover.proto", "core/contract/common.proto"];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
    let proto_root = PathBuf::from(manifest_dir).join("proto");

    for file in ROOTS.iter().chain(IMPORTS.iter()) {
        println!("cargo::rerun-if-changed=proto/{file}");
    }

    let descriptors = protox::compile(ROOTS, [&proto_root])?;
    let mut config = prost_build::Config::new();
    config.disable_comments(["."]);
    config.compile_fds(descriptors)?;
    Ok(())
}
