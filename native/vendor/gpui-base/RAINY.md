# gpui-base 0.8.0, as Rainy builds it

A copy of the crates.io package `gpui-base` 0.8.0 (part of GPUI Kit, https://github.com/longbridge/gpui-kit,
Apache-2.0, see `LICENSE-APACHE`), used through `[patch.crates-io]` in `native/Cargo.toml`. GPUI Kit 0.8.0 pins this
exact version.

Changes from the published package:

- `Cargo.toml`: the `[[bench]]` targets and the `criterion` dev-dependency are removed, and so is `benches/`.
- `src/input/base/state.rs`, `indent_of_next_line_at`: the indentation of a new line comes from the logical line,
  not from the row a soft wrap draws the cursor on. Enter at the end of a wrapped line started the new line at
  column 0 (the last row of a wrapped line has no indentation), and the smart-indent rules saw only that row.
- `src/input/editor/lsp/overlay.rs`, `insert_completion`:
  - an item's `insertText` replaces the typed word as its label does; it went in at the cursor beside the word, so
    `PRI` with `PRIMARYLANGID` chosen became `PRIPRIMARYLANGID`;
  - an item's `additionalTextEdits` (the import line of an automatic import from ty, for one) are applied with it as
    one undo step, and the cursor ends after the completion; they were dropped.

Each change is marked `Rainy:` where it is made. To move to another gpui-base, copy the new package here the same way
and apply the changes again, or drop them once the package has its own.
