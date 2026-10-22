//! `package protocol;` as prost generates it from the vendored `.proto` files.
//! See `build.rs` for provenance.
//!
//! The generated types stay crate-private. The contract discriminants, message
//! names and result words that are wire format are restated as
//! [`crate::raw::Contract`] and [`crate::shape::ContractResult`], and pinned
//! against this module by a test beside each.

#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    dead_code,
    missing_debug_implementations,
    unnameable_types,
    unreachable_pub,
    unused_qualifications,
    rust_2018_idioms,
    non_snake_case,
    non_camel_case_types,
    reason = "generated code, not written to this workspace's standards"
)]
pub(crate) mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
