# Foxy epaint patch

The Rust source is from the egui 0.37.0 bidirectional text backport at revision
`f7e89e23a1639cc4fdf232edd6f9270f167711e5`. The manifest comes from the published
epaint 0.37.0 package with the backport's `unicode-bidi` dependency.

Foxy restores logical character metadata for shaped RTL clusters. The upstream
backport gave marks in some clusters the same character metadata as the base
letter. The fix preserves the original marks for text editing and AccessKit
without changing the shaped glyphs.

Validate a replacement with `cargo test --locked -p Foxy --bin Foxy accesskit_audit::rtl`.
See [egui PR 8577](https://github.com/emilk/egui/pull/8577) and the
[0.37.0 backport](https://github.com/richerfu/egui/commit/f7e89e23a1639cc4fdf232edd6f9270f167711e5).
