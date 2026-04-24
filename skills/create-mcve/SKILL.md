---
name: create-mcve
description: Build a Minimal, Complete, Verifiable Example that reproduces a suspected bug in an external library or dependency. Use when the user wants to isolate a third-party defect, file an upstream issue, demonstrate a regression, or confirm a root cause outside of a larger application. Works for any language or ecosystem (Rust, Node, Python, Go, Ruby, etc.).
---

# Create an MCVE

An MCVE is a standalone, runnable project that demonstrates one specific bug. Each case
lives in its own directory so it can be shared, committed, and reproduced independently.
See <https://stackoverflow.com/help/minimal-reproducible-example> for the underlying
principle.

## Core principles

   - **Minimal**: remove every line that is not required to trigger the bug. No unused
     dependencies, features, config, or code paths.
   - **Complete**: someone else can clone the repo, run one command from the case
     directory, and observe the same failure.
   - **Verifiable**: the program prints clearly whether the bug reproduced or not. Do not
     rely on the reader to interpret a stack trace.
   - **Pinned**: lock every external dependency to an exact version or commit SHA so the
     repro does not drift.
   - **Isolated**: one bug per case. If you find a second issue while investigating, make
     a new case.

Note: Some repros run in a terminal, others may run in a browser, and yet others may be
non-executable, so apply the instructions below on a case-by-case basis. The core
principles above always apply.

## Directory layout

Place each case under `cases/` with a zero-padded numeric prefix and a short slug:

```
cases/
└── NNN-<short-slug>/
    ├── README.md              # required, see structure below
    ├── <package manifest>     # Cargo.toml, package.json, pyproject.toml, go.mod,
    |                          # Gemfile, etc.
    ├── <lockfile>             # commit it — pinning is the point
    └── <source files>         # minimal code needed to reproduce
```

Pick the next unused `NNN` by listing existing cases. The slug should name the library and
the defect, e.g. `002-axios-retry-drops-headers`, not `002-weird-bug`.

The package manifest and lockfile must use whatever is idiomatic for the target ecosystem.
Follow the conventions of the library being reproduced against (its README, its CI config)
so the repro environment matches what maintainers expect.

## README.md structure

Every case's README.md must have these sections, in order:

1. **Title**: `# Bug Repro: <one-line summary of the observed behavior>`
2. **Problem**: 2–5 sentences. What the user-visible symptom is, under what conditions.
   Link the offending library by name and pinned version/SHA.
3. **Root cause** *(if known)*: walk through the exact code path that produces the bug.
   Link to specific lines in the upstream repo using permalinks (commit SHA or version
   tag, never `main`). If the root cause is still unknown, write "Unknown — see repro
   output" and skip to the next section.
4. **Expected behavior**: one paragraph describing what should happen instead. Be specific
   — "it should roll back the transaction before returning the connection to the pool,"
   not "it should work."
5. **How to run**: the single command the reader should execute from the case directory.
   Example:
   ```
   cargo run --bin <binary>
   ```
   or `npm start`, `python repro.py`, `go run .`, etc.
6. **Relevant source files**: a markdown table mapping upstream file paths → line ranges →
   what that code does. Every entry links to a permalink pinned to the same version/SHA
   referenced in the Problem section. Group by repository when multiple upstream projects
   are involved.

## Code conventions

   - **Pin everything**. Use exact versions in the manifest (`=1.2.3`, `"1.2.3"` not
     `^1.2.3`, `rev = "abc1234"` not a branch). Commit the lockfile.
   - **No filesystem side effects outside a tempdir.** Use the ecosystem's temp-dir
     utility (`tempfile` crate, `tmp` module, `os.makedtemp`, `os.MkdirTemp`, etc.) and
     clean up at the end.
   - **Narrate the run.** If the repro runs inside a terminal, print a header for each
     step (`=== Step 1: setup ===`) and log what the step observed. A maintainer reading
     the output should see the failure without attaching a debugger.
   - **Probe before reproducing.** When the bug is about internal state (leaked locks,
     dangling transactions, zombie handles), add a probe that demonstrates the bad state
     *before* the user-visible failure. This makes the root cause obvious rather than
     inferred.
   - **End with a verdict.** The final lines must print either
     `BUG CONFIRMED: <one-sentence restatement>` or `BUG NOT REPRODUCED: <likely reason>`.
     Base this on an explicit check in the code, not on whether a panic/exception fired.
   - **No unrelated features.** No logging frameworks, CLI parsing, config files, or
     abstractions unless they are required to trigger the bug. If you catch yourself
     adding a helper that might be useful later, delete it.
   - **Prefer the library's public API.** Reproduce against the surface users actually
     call. Only reach into internals when the bug is only observable there, and say so in
     the README.

## Control experiments

If a step in the repro could be dismissed as "maybe your probe is wrong, not the library,"
add a second binary/script in the same case that proves the probe behaves correctly
against a known-good baseline. Reference it from the main repro's comments
(`see ./src/bin/prove_acquire_blocks.rs`). This pre-empts the first round of pushback on
an upstream issue.

## Verification checklist

Before declaring the case done:

   - [ ] `git clone` + the documented run command reproduces the failure on a clean
     machine (or a fresh checkout).
   - [ ] Removing any one source line, dependency, or step causes the bug to stop
     reproducing or the case to stop compiling. If not, it is not yet minimal.
   - [ ] The README's upstream links all point at the same pinned version/SHA as the
     manifest.
   - [ ] The verdict line accurately reflects the observed behavior on a fresh run.
   - [ ] Lockfile is committed.

## Reference example

See `cases/001-sqlx-sqlite-conn-mgr-interruptible-txns/` for a worked Rust example that
follows every convention above: numbered directory, pinned git revs in `Cargo.toml`,
stepwise `println!`-narrated run, internal-state probe plus user-visible repro,
control-experiment binary in `src/bin/`, and a README with Problem / Root cause /
Expected behavior / How to run / Relevant source files.
