# Contributing

Thanks for looking. Bug reports, measurements on hardware we do not have, and engine adapters are the most useful things
right now.

## Reporting a problem

Run `flelink report MODEL --out report.json` and attach the file. It holds the machine summary, the engine found, the plan and
the checks; tokens and keys are removed and your home folder is shown as `~`. Say what you ran, what you expected and what
happened.

## Measurements

Speed numbers are only comparable when they are measured the same way. Use `flelink bench --url http://127.0.0.1:8080
--runs 3 --output bench.json` against a running `flelink run MODEL`, and give the GPU, RAM, model file and FLE Link version.

## Code

1. `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` must be clean.
2. `cargo test --all` must pass. With `FLE_STUDIO_SRC` and `FLE_ENGINE_SRC` set (see the README), the tests also compare this
   program with the Python Studio and helper request by request. A deliberate difference goes into `DEVIATIONS.md` with the
   reason, and the test learns about it.
3. A change to how FLE Link behaves when something goes wrong gets a case in `tests/fulltest/fulltest.py`.
4. Keep the engine behind the adapter (`crates/flelink-core/src/adapter.rs`): engine-specific rules belong there, not in the
   callers.

Small pull requests with one purpose are reviewed fastest. For a larger change, open an issue first so we can agree on the
shape.

## Licence

By contributing you agree that your contribution is licensed under the MIT licence of this repository.
