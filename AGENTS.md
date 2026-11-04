# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`sample-lines` is a Rust CLI, published to crates.io under that name, that builds a binary named `samp`. It randomly samples lines from a file or stdin in one pass.

## Common commands

```bash
cargo build                # Debug build → target/debug/samp
cargo build --release      # Release build (fat LTO, codegen-units=1)
cargo build && cargo test  # Run all tests (see caveat below)
cargo test <name>          # Run tests matching a substring (rebuild first)
cargo clippy --all-targets # Lint (plain clippy skips test code)
cargo fmt                  # Format
```

**Test caveat:** most tests spawn the `samp` binary as a subprocess, and `cargo test` does not build that binary. Always run `cargo build` first: without it, those tests silently run against the old binary and can pass or fail for the wrong reason. `find_executable()` uses paths relative to the repo root and checks `target/debug/samp` before `target/release/samp`, so `cargo build --release` does not help while a stale debug binary exists.

All tests live in `mod tests` at the bottom of `src/main.rs`; there is no `tests/` directory.

## Architecture

Single-file program (`src/main.rs`) with two sampling strategies dispatched from `main`:

- `reservoir_sample` (`-n <NUM>`): fixed sample size. Keeps the first `k` lines, then replaces a random slot in the reservoir with probability `k/(total+1)`, where `total` is the 0-based line index. Output is **not** in input order.
- `probability_sample` (`-r <RATE>`): independent Bernoulli trial per line. Output size is non-deterministic, and output stays in input order.

Both collect results in a `Vec` and write only after reading all input. Memory use scales with the sample size, not the input size, so a high `-r` on a huge input holds most of it in memory.

`-n` and `-r` are mutually exclusive (clap's `conflicts_with`). Exactly one is required, which `main` checks manually after `parse_args`.

**Headers (`-p <NUM>`):** consumed from the line iterator and printed before sampling, so the sampling functions only see post-header lines. The number is required, mirroring `head -n`.

**Broken pipes:** `write_line` exits 0 on `ErrorKind::BrokenPipe`, so `samp -n 1000000 huge.log | head` stays quiet. All stdout output, headers included, must go through it, because `println!` panics on a closed pipe. `write_results` uses a `BufWriter` and needs its explicit flush, since dropping one ignores write errors. Headers skip the `BufWriter` so they appear before the input has been fully read.

**Raw bytes:** `byte_lines` yields each line as `Vec<u8>`, so input doesn't need to be valid UTF-8 and output is byte-identical. Only the trailing `\n` is removed, which keeps the `\r` in CRLF input. `write_line` adds `\n` back, so a final line with no newline gains one; this matters because reservoir output can move that line. Don't reintroduce `String` or `BufRead::lines()`.

**Determinism:** `--seed` gives reproducible output; without it, `StdRng::from_entropy()` is used. `test_probability_sample_reproducibility` asserts exact output tied to the `rand` 0.8 PRNG, so bumping `rand` will likely break it.

## Repo notes

- CI (`.github/workflows/ci.yml`) runs fmt, clippy with warnings denied, build, then tests on pushes to `main` and on pull requests.
- `kk.py` and `ll.py` in the repo root are untracked scratch scripts that print large random CSVs to stdout (100M and 10M rows). Don't commit them or any CSVs generated from them; only `/target` is gitignored.
