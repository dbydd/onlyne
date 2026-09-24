# Packaging

Service definitions ship as examples. An operator or a package installs them. The core binaries
never install or launch themselves as a service.

## Files

| file | target |
| --- | --- |
| `launchd/se.onlyne.server.plist` | `~/Library/LaunchAgents/` or `/Library/LaunchDaemons/` |
| `systemd/onlyne-server.service` | `/etc/systemd/system/` |
| `systemd/onlyne-client@.service` | `/etc/systemd/system/` |

## Paths to edit

Both shapes run one foreground command. Neither carries supervisor logic of its own:

- server: `onlyne-server run --root <server-root>`, working directory `<server-root>`
- client: `onlyne-client run --workspace <workspace>`, working directory `<workspace>`

The units carry `/srv/onlyne` and `/srv/onlyne-ws/%i` as the two roots. Replace them with the real
roots before loading. Create a root first with `onlyne-server init --root <dir>`; that command also
makes `.onlyne/logs/`, the directory the log files land in.

## Restart behavior

On macOS, `KeepAlive` with `SuccessfulExit=false`; on Linux, `Restart=on-failure`. Both restart the
daemon after an abnormal exit, and both leave a clean exit alone. `systemctl reload onlyne-server`
sends SIGHUP. The daemon re-parses `spec.toml`, then swaps it atomically.

## Logs

Daemon output goes to two places: `.onlyne/logs/server.log` under the server root, and
`.onlyne/logs/client.log` under the role workspace.

## Client instances

The client unit is a template. `systemctl start onlyne-client@planner` runs the client for
`/srv/onlyne-ws/planner`. A nested workspace name uses `systemd-escape` output as the instance name.

## Releases

`github.com/dbydd/onlyne/.github/workflows/release.yml` describes a planned binary-release
pipeline. It did not run for v1.4.0, so the registry packages are the current installation channel.
For a future binary release, push a tag that names the workspace version — for example
`git tag v1.4.0 && git push origin v1.4.0` — or run the workflow by hand with a `tag` input. Four
jobs run in order:

1. `verify` — the same gate as `ci.yml` (`cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), plus a check
   that the tag matches `[workspace.package].version`. A release never ships red, and a tag that
   names no workspace version fails rather than publishing an unreproducible build.
2. `build` — five targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`. Each builds
   the five release binaries (`onlyne`, `onlyne-server`, `onlyne-client`, `onlyne-gateway`,
   `onlyne-tui`) with `--locked`, packages them beside `LICENSE` and the shell completions the CLI
   emits itself, and writes a `.sha256` beside each archive. Every job installs `protoc`: the
   gateway plugins' default features pull `openlark`, whose build runs prost.
3. `release` — publishes every archive, its checksum, and a combined `SHA256SUMS` to the GitHub
   Release, creating it with generated notes when the tag has none.
4. `formula` — renders `packaging/homebrew/onlyne.rb` from that release's checksums and commits it
   back to `main`, so the committed formula always describes a release that exists.

## Homebrew

The formula installs the prebuilt binaries, so a user needs no Rust toolchain. `onlyne-gateway` is
one of the five, which is what makes `brew install onlyne` a complete install rather than a partial
one. After the first release:

```sh
brew install dbydd/onlyne/packaging/homebrew/onlyne.rb   # or a tap that carries the same file
```

The local `packaging/homebrew/onlyne.rb` still targets v1.3.1 and carries
`REPLACE_ON_FIRST_RELEASE`; it is not a v1.4.0 installation source. A real release workflow run
would render it from that release's checksums. `scripts/render-formula.py` is the only writer and
refuses to write a formula when a brew platform's archive is missing from the `SHA256SUMS` input.

## Install script

`packaging/install.sh` is the no-Homebrew path. It resolves this machine's target, downloads the
matching archive and `SHA256SUMS`, refuses to install anything whose checksum is absent or wrong,
and puts the five binaries in `PREFIX/bin` (`/usr/local` by default). It writes nothing else and
starts no service. `sh packaging/install.sh <tag>` pins one future binary-release tag; v1.4.0
registry installation uses the Cargo command in the root README.
