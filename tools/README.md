# Mutation runners

Each runner weakens one check at a time in our own code, runs the tests that should notice, and puts the file back. Every mutant has to make a test fail. A mutant that doesn't compile is reported as `NO BUILD` and counted as surviving, since it proves nothing about the tests. Each runner first runs the tests unmodified and stops if they fail, so a failing test can't make every mutant look caught.

They edit files in the working tree and restore them when they finish, so run them on a clean checkout, one at a time, and not while another build or test runs in the same checkout. The third argument is a directory that receives a copy of every file before it is changed. They pass `--offline --locked` to cargo, so run the unit or sandbox tests once first to fetch the crates.

`tools/clean` runs a command with only `PATH`, `HOME`, `CARGO_HOME` and `RUSTUP_HOME`, so no build script sees your API keys or tokens.

| Command | Weakens | Runs | Prints last (12 Nov) | Time here |
| --- | --- | --- | --- | --- |
| `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit` | the guard's checks in `contracts/guard/src/lib.rs` and `types.rs` | `cargo test -p near-spend-guard` | `91 mutants; survived: none` | 7 min, beside the client pass |
| `python3 -I tools/mutate_sandbox.py . tools/clean /tmp/nsg-mutants/sandbox` | what only the compiled contract shows: near-sdk's export wrapper, settlement against the test token, the burst, gas headroom, the window, the demo's naive guard | one sandbox test file per mutant, through `scripts/test-sandbox.sh` | `31 mutants; survived: none` | 18.5 min (each mutant rebuilds the wasm) |
| `python3 -I tools/mutate_client.py . tools/clean /tmp/nsg-mutants/client` | every file in `client/src` and `client/deploy/world.ts` | `node --test` on `client/test`, plus `client/test-near` for `near.ts` (needs `npm ci --ignore-scripts` in `client/` first) | `173 mutants; survived: none` | 7.5 min (each `near.ts` mutant runs `test-near/` too) |

Each line before the last says `caught` (with the tests that failed) or `SURVIVED`. A fourth argument runs only the mutants whose name contains it, for example `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit "daily cap"`.

The sandbox runner needs what `scripts/test-sandbox.sh` needs: Linux x86_64 or an Apple Silicon Mac. Every new check in the code gets a mutant here in the same commit.
