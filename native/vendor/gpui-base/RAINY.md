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

- `src/text/node.rs`, `CodeBlock::render`: a code block with actions (`TextView::code_block_actions`) gets the same
  room above its first line as one with a language label; the actions covered the end of the first line.
- `src/text_selection.rs`, `paint_text_selection`:
  - the `MouseUp` listener finishes selection and resets `mouse_down_prepared` in the capture phase instead of bubble,
    so a child element stopping propagation in `on_click`/`MouseUp` cannot deadlock `is_selecting` and
    `mouse_down_prepared`;
  - the `MouseMove` listener requires `event.pressed_button == Some(MouseButton::Left)` before extending a selection
    and automatically calls `end` / resets `mouse_down_prepared` on capture when moving without the primary button held
    (e.g. after a missed `MouseUp` outside the window).
- `src/motion/reveal.rs`, `MotionReveal::request_layout`: a reveal does not shrink (`flex_shrink` 0). In a column
  holding more than its height (the chat list in the sidebar with many chats), flex shrank every reveal in it, so
  each group showed only part of its rows and the list never scrolled.
- `src/switch.rs`, `Switch::render`: an enabled switch calls `GlobalState::suppress_text_selection` on primary mouse
  down (matching `Button`), so clicking a switch over selectable text does not start window-level text selection.

Each change is marked `Rainy:` where it is made. To move to another gpui-base, copy the new package here the same way
and apply the changes again, or drop them once the package has its own.
