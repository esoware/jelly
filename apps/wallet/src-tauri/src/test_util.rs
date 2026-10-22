use std::path::Path;

use specta_typescript::Typescript;

use crate::surface::builder;
pub use crate::surface::{COMMAND_NAMES, EVENT_NAMES};

pub fn export_bindings(path: &Path) -> Result<(), specta_typescript::Error> {
    builder().export(Typescript::default(), path)
}
