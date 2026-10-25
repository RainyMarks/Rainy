# gpui-pre 0.3.9, as Rainy builds it

A copy of the crates.io package `gpui-pre` 0.3.9 (Zed's GPUI, snapshot of zed@93d7456, Apache-2.0, see
`LICENSE-APACHE`), used through `[patch.crates-io]` in `native/Cargo.toml`. GPUI Kit 0.8.0 pins this exact version.

Changes from the published package:

- `Cargo.toml`: the `[[example]]` targets are removed, and so is `examples/`.
- `src/text_system/line_wrapper.rs` and `src/text_system/line_layout.rs`: Chinese and Japanese line breaking. GPUI
  allows a break before every CJK character, so a closing mark such as `。` or `，` could start a line on its own and
  an opening one such as `（` or `《` could end one. Closing punctuation now counts as part of the word before it
  (`is_word_char`), and there is no break right after opening punctuation (`is_opening_punctuation`). Both changes
  are marked `Rainy:` where they are defined.

To move to another gpui-pre, copy the new package here the same way and apply both changes again.
