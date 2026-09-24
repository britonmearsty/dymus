# Contributing to Dymus

Thanks for considering contributing! This is a small, single-maintainer project, so please be friendly and keep changes focused.

## Reporting a bug

Open an [issue](https://github.com/britonmearsty/dymus/issues) with:

- the `dymus --version` output,
- your terminal type and size (e.g. Alacritty, 120×40),
- steps to reproduce, and
- any log output (run with `RUST_LOG=debug dymus` if it helps).

## Suggesting a feature

Open an issue describing the problem you're trying to solve. Proposals that fit the "keyboard-driven terminal client" focus are most likely to land.

## Making a pull request

1. Fork the repo and create a branch off `master`.
2. Keep changes focused: one logical change per PR, with a clear title.
3. Update the README if behavior or controls change.
4. Add or update tests where practical (see the optional integration tests below).
5. Verify everything passes before opening the PR:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test

   # Optional: mpv integration (generated silence, null audio output)
   cargo test mpv_reports_playback_and_eof_with_local_audio -- --ignored
   # Optional: live network tests
   cargo test live_search_resolve_and_stream_audio -- --ignored --nocapture
   cargo test live_radio_returns_related_tracks -- --ignored --nocapture
   ```

6. Push and open the PR. The CI workflow runs the fmt, clippy, and test commands above, so a green PR is expected.

## Reviewing

Maintainers review PRs, run the checks locally, may test the GUI manually, and merge. Please be patient — small hobby projects move at their own pace.

## Code of conduct

Be respectful. Harassment and trolling are not welcome. This project is small, so conflicts are rare; if something feels wrong, open an issue or email the maintainer.