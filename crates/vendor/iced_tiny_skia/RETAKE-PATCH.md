# iced_tiny_skia 0.13.0, patched for Retake

A copy of [`iced_tiny_skia` 0.13.0](https://crates.io/crates/iced_tiny_skia/0.13.0) from the
[iced](https://github.com/iced-rs/iced) project (MIT, see `LICENSE`), used through
`[patch.crates-io]` in the workspace `Cargo.toml`.

One change, in `src/lib.rs` (two places): canvas and text groups were drawn with
`group.transformation() * Transformation::scale(scale_factor)`, which scales a group's
contents but not its position, so at Windows display scaling above 100% every canvas (the
clip timeline, the montage editor) is drawn away from where it belongs. The patch multiplies
in the other order: `Transformation::scale(scale_factor) * group.transformation()`.

Drop this copy when moving to an iced release with the fix.
