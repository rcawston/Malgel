# Contributing to Malgel

Thanks for helping improve Malgel. Bug reports, ideas and pull requests are
all welcome.

## Reporting a problem

Open an issue with what you did, what you expected and what happened
instead. Include your operating system and version, the Malgel version
(Help → About Malgel), and, if something renders wrongly, a small Markdown
file that shows it.

## Pull requests

Before opening one, make sure these pass, as CI checks them:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Keep each pull request to one change, and describe what it changes and why.

## Contributor License Agreement

Malgel is licensed under the [Apache License, Version 2.0](LICENSE). To
accept a contribution, we need your agreement to the
[Malgel Contributor License Agreement](CLA.md), based on the Harmony
Individual Contributor License Agreement. You keep the copyright in your
contribution. The agreement gives Ross Cawston, who maintains Malgel, a
license to use and relicense it, and always to offer it under Malgel's
license at the time you contributed.

You agree once, on your first pull request: the CLA Assistant bot comments
on it, and you reply with

> I have read the CLA Document and I hereby sign the CLA

Your agreement is recorded in `signatures/cla.json`, and covers your later
pull requests too, unless the agreement changes.

### If you don't own all of your contribution

The agreement covers work you own. If your contribution includes work you
didn't write (for example code copied from another project, or work your
employer owns), say so in the pull request: name the source and its
license, or have the owner (such as your employer) confirm they agree. If
you are contributing on behalf of a company, open an issue first so we can
arrange an agreement with the company instead.
