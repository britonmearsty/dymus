## What does this change do?

<!-- Describe the problem and the resulting user-visible behavior. -->

## Related issue

<!-- Link an issue if one exists. This is optional for small fixes. -->

## Validation

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `cargo build --release --locked`
- [ ] `sh scripts/check-manpage.sh ./target/release/dymus` (when available)

## Documentation and user impact

- [ ] Updated relevant README, configuration, or man-page docs.
- [ ] Described changed controls or behavior above.
- [ ] Removed credentials and private data from logs and screenshots.
