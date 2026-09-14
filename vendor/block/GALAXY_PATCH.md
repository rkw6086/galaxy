# Local compatibility patch

Source: crates.io `block` 0.1.6 by Steven Sheldon, licensed MIT.

Replaces the uninhabited opaque `Class` enum with an opaque C-layout struct.
Only pointers to this type cross the ABI; no storage is read or constructed in Rust.
This fixes Rust's `uninhabited_static` warning without suppressing the lint.
The previously implicit C ABI is also written explicitly on extern declarations.

The upstream manifest references an unpublished local test utility; that dev-only
entry is omitted. The library source and README otherwise match the published crate.
