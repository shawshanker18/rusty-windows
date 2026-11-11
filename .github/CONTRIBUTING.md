# Contributing

Contributors are welcome, whether it is one pixel or a whole new window.

## Ways to help

- **Measure.** Run it on your PC and open an issue with your Windows build, screen size and the
  memory numbers from `scripts/mem-probe.ps1`. One PC is not enough data.
- **Report.** Anything that looks or behaves differently from Windows is a bug here. A screenshot of
  both helps most. Remove anything personal from screenshots first.
- **Fix pixels.** Pick a window from [FEATURES.md](../docs/FEATURES.md) that is below target and close the gap.
- **Build something missing.** See "Not done yet" in docs/FEATURES.md.

## Good first tasks

- Measure the match of a shell piece marked "not measured" and add the number to docs/FEATURES.md
- Light theme for Calculator
- A friendly message in the Changes pane when the folder is not a git repository
- Notepad: spell-check squiggles with the Windows spell checker
- Pinned apps on the taskbar

## Rules of the code

1. **Small and plain.** Standard library and the `windows` crate first. No new dependency for what a
   few lines can do. No frameworks, no web views in the shell.
2. **Live state only.** Nothing shown may be hard-coded; read it from Windows.
3. **One check per piece of logic.** A unit test in the same file (`cargo test --release`) or a
   `--…-selftest` switch.
4. **Never hold the state borrow across a call that can send window messages** (`SetWindowPos`,
   `SendMessage`, `SetWindowText`, `SetFocus`, menus, dialogs). Copy the handles out first. The
   release build aborts on panic, so a double borrow takes the whole shell down.
5. **Slow system calls go on their own thread**, never the one that draws.
6. **Be honest in the docs.** Write the measured number, and write "not measured" or "not done"
   when that is the truth.

## Pull requests

- One topic per pull request, with what you tested and on which Windows build
- `cargo build --release` without warnings and `cargo test --release` passing
- For visual changes: before and after pictures, and the match number if you measured it
- Tests must not press a global Esc or Enter, overwrite the clipboard, or delete or rename real user
  data. Post messages to the project's own windows instead.

By contributing you agree your work is released under the [MIT licence](../LICENSE).
