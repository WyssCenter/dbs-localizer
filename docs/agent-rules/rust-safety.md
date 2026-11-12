# Rust safety rules (Wyss DBS Localizer)

**Applies to:** `crates/**`

## Clippy lints

Every crate's `Cargo.toml` must include:

```toml
[lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
```

`unwrap()` and `expect()` in production paths cause uninformative panics. Use `?`, `ok_or_else`, `unwrap_or_else`, or `match`.

Acceptable exceptions (annotate with `#[allow(clippy::unwrap_used)]` and a comment):
- Test code
- Mutex locks on the UI thread where poisoning would mean the app is already broken
- Compile-time constants with provably valid values

## Mutex guards

`lock().unwrap()` in background threads must become `lock().unwrap_or_else(|p| p.into_inner())` or the thread must propagate an error. A panicking background thread poisons the mutex and cascades panics to every subsequent `unwrap()`.

## NIfTI dimension guard

Before allocating any volume buffer from a NIfTI header, validate:

```rust
if n0 > 2048 || n1 > 2048 || n2 > 2048 {
    return Err(VizError::NiftiLoad(format!("dims too large: {n0}×{n1}×{n2}")));
}
```

A malformed header with dims like `65535^3` would allocate > 1 TB before the file read fails.

## Path handling

All paths from user-supplied sources (JSON manifest, CLI args, BIDS config) must be canonicalized before use:

```rust
let p = std::fs::canonicalize(&raw_path)
    .map_err(|e| VizError::NiftiLoad(format!("bad path {}: {e}", raw_path.display())))?;
```

Reject paths containing `..` components before canonicalization to fail fast on traversal attempts.

## Filename sanitization

Cache filenames derived from user data (ROI labels, subject IDs) must use a strict allowlist:

```rust
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(128)
        .collect()
}
```

## Error types

`VizError` and other error enums must not use bare `String` for error bodies when a structured variant is possible. Prefer typed variants (`NiftiDimsTooLarge { n0, n1, n2 }`) so callers can pattern-match.

## Comments

Keep comments to the strict minimum:
- Non-obvious algorithmic decisions
- Invariant contracts the type system cannot express
- References to spec sections (`// specs/03 §4.2`)
- `// SAFETY:` for any `unsafe` block

Remove comments that restate what the code does, describe loop structure, narrate imports, or repeat the function name.
