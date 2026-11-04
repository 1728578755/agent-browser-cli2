## Summary

<!-- What changes, and why. Link the issue if one exists. -->

## Scope

- [ ] Rust daemon / CLI (`src/`)
- [ ] Chrome extension (`assets/`)
- [ ] npm packaging (`npm/`, `scripts/`)
- [ ] Documentation
- [ ] CI / release

## Testing

<!-- Commands you ran. Do not paste secrets or private page content. -->

```shell
atb --version
```

## Checklist

- [ ] I ran `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets --all-features`.
- [ ] I ran `node --test tests/*.test.mjs` or explained why it does not apply.
- [ ] New user-facing commands and flags are documented.
- [ ] I did not commit generated files under `npm/platform/`, `dist/`, or `target/`.
- [ ] I kept `atb` as the command name in examples.
