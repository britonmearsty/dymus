# Contributing to Dymus

Thanks for helping improve Dymus. The project welcomes bug reports, focused feature proposals, and pull requests for its keyboard-driven terminal music player.

## What makes a contribution a good fit

- It addresses a clear bug or improves a real user workflow in the terminal player, headless player, or local library.
- The change is focused and has a clear description of its behavior and tradeoffs.
- Relevant tests and documentation are updated. User-visible behavior, commands, configuration, and controls should be documented where appropriate.
- It does not include credentials, browser cookies, personal data, or generated build artifacts.
- Contributions are submitted under the project's MIT license.

A passing CI run is required before merging. The `CI / test` job checks formatting, Clippy, tests, the release build, and the bundled man page. Maintainers make the final decision on scope and merging; passing CI alone does not guarantee acceptance. There is no fixed review-count requirement, so a maintainer may merge after reviewing and addressing any feedback.

## Reporting a bug

[Open an issue](https://github.com/britonmearsty/dymus/issues/new) with the `dymus --version` output, your terminal and its size, steps to reproduce, and relevant logs. Check logs before sharing to ensure they contain no credentials or private data.

## Suggesting a feature

[Open an issue](https://github.com/britonmearsty/dymus/issues/new) explaining the problem and who it affects. Discuss substantial changes before investing in a large implementation; proposals that fit the terminal music-player focus are most likely to land.

## Opening a pull request

1. Fork the repository and branch from `master`.
2. Keep each pull request focused on one logical change. Link a related issue when there is one; an issue is not required for a small fix.
3. Explain the problem and resulting behavior. Include terminal details or screenshots when they help reviewers assess a UI change.
4. Update relevant documentation and tests.
5. Run the checks that apply to your changes:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --locked
   cargo build --release --locked
   sh scripts/check-manpage.sh ./target/release/dymus
   ```

   The manual check requires `groff`. GitHub Actions runs all of these checks on pull requests. Optional mpv and live-network integration tests are described in the README.
6. Respond to review feedback and make sure required CI checks pass. Maintainers review and merge contributions.

## Conduct

Treat contributors and users respectfully. Harassment and trolling are not welcome. If a concern comes up, raise it in the issue tracker or contact the maintainer.
