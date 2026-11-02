# Contributing

Pull requests are welcome, from a typo to a feature such as Focus (see Upcoming in the README). A well-made one:

- **Does one thing.** One feature or fix per pull request, with a short description of what changes for the person
  using Livae.
- **Puts shared logic in `livae-core`, with tests.** Parsing, ranking, scheduling and geometry live there and run on
  any system; `livae-mac` and `livae-win` only draw and talk to the system.
- **Works on both systems** where the feature makes sense, or says clearly which one it covers.
- **Follows `DESIGN.md`**: true black, Livae Sans, the existing text styles and spacing, no new colours.
- **Stays small and fast.** No new dependency without a reason in the description; nothing slow on the main thread.
- **Passes the checks**: `cargo test -p livae-core`, `cargo clippy -p livae-core --all-targets` with no warnings, and a
  build of the front end you changed (CI runs all of them).
- **Respects privacy.** Livae keeps everything on the person's machine. Anything that watches what they type is off
  until they turn it on; anything that goes online does so only for something they asked (money rates are fetched
  after a money question, not before).
