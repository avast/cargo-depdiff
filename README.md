# cargo-depdiff

If you do `cargo update`, add dependencies to the project or update something
manually, the dependencies recorded in `Cargo.toml` change and you get a report
of what changed right at that time. But when browsing the history, reading what
changed from the diff of `Cargo.toml` is inconvenient. This makes reviewing pull
requests a bit harder than necessary.

The `cargo depdiff` command tries to help in this situation a bit. You can point
it to a git commit, commit range or similar (or run in a directory with
uncommited changes) and see a similar report.

Furthermore, some additional information may be requested (changes to authors,
extracting changelogs, etc).

It is currently in an early stage, so bugs, bad formatting and missing pieces of
information are possible. Pull requests for anything of that are indeed welcome,
as are issues describing use cases we haven't thought about, bug reports, etc.

## Example

```
cargo depdiff 9d06984055be56a76e8c365292e7d840da9e7515
+++ adler 0.2.3
+++ aho-corasick 0.7.13
+++ bitmaps 2.1.0
+++ bstr 0.2.13
+++ bytesize 1.0.1
...
```

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
