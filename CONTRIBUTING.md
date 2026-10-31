# Contributing to Dymus

Dymus is a keyboard-driven terminal music player. Contributions are welcome, and the project is currently maintained by a single author, so the workflow stays deliberately simple.

## How changes land

- The maintainer commits directly to `master` and pushes when a change is ready. There is no required pull request, review, or approval step.
- Outside contributors may open a pull request from a fork, but it is optional. An issue describing the problem is just as useful.
- Pushes and merges to `master` are not blocked by branch protection.

## Checks

CI runs on every push to `master` and on every pull request. It is there to catch problems, not to gate your push. Before pushing, it is worth running whichever checks apply to your change:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --release --locked
sh scripts/check-manpage.sh ./target/release/dymus
```

The man-page check needs `groff`. Optional mpv and live-network integration tests are described in the README. If a check fails after a push, fix it in a follow-up commit — nothing is blocked.

## What makes a change a good fit

- It fixes a clear bug or improves a real workflow in the terminal player, headless player, or local library.
- It is focused and has a clear description of its behavior and tradeoffs.
- Relevant tests and documentation are updated. User-visible behavior, commands, configuration, and controls should be documented where appropriate.
- It does not include credentials, browser cookies, personal data, or generated build artifacts.
- Contributions are submitted under the project's MIT license.

## Reporting a bug

[Open an issue](https://github.com/britonmearsty/dymus/issues/new) with the `dymus --version` output, your terminal and its size, steps to reproduce, and relevant logs. Check logs before sharing to ensure they contain no credentials or private data.

## Suggesting a feature

[Open an issue](https://github.com/britonmearsty/dymus/issues/new) explaining the problem and who it affects. Substantial changes are easier to discuss before investing in a large implementation.

## Conduct

Treat contributors and users respectfully. Harassment and trolling are not welcome. If a concern comes up, raise it in the issue tracker or contact the maintainer.
