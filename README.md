# opencode-turbo

A tiny native launcher that makes [opencode](https://opencode.ai) start fast on
Android/Termux.

`opencode-turbo` replaces the per-launch work of the `opencode-termux` shell
wrapper (Node startup, dependency checks and an npm registry query on every
launch) with direct path resolution and `execve`. It also turns on opencode's
own `OPENCODE_FAST_BOOT` mode by default, so the terminal UI does not sit behind
the loading screen while the rest of the app initializes.

The real opencode runtime (the large Bun binary in `~/.opencode`) is **not**
patched or modified.

## Results

Measured on an aarch64 Android 15 device in Termux with warm caches,
`opencode-termux` 1.19.3, medians of 2-3 runs:

| What | Before | opencode-turbo | Improvement |
| --- | ---: | ---: | ---: |
| `opencode --version` | 2964 ms | 12 ms | ~247x |
| `opencode --help` | 3024 ms | 13 ms | ~233x |
| `opencode debug startup` (wall) | 2972 ms | 1284 ms | 2.3x |
| TUI first paint | 3660 ms | 1930 ms | 1.9x |
| TUI home screen | 9205 ms | 3355 ms | 2.7x |

For reference, `opencode-turbo --turbo-no-fast-boot` shows the TUI home screen
in about 7.3 s, so opencode's fast boot alone removes roughly 4 s of perceived
startup and the native launcher removes another ~1.8 s.

The TUI still needs about 2 s to initialize the Bun runtime and the terminal
renderer. Removing that would require forking opencode itself; this project
deliberately stays compatible with normal `opencode-termux` updates.

## Install

Requirements:

- aarch64 Termux (the only ABI supported by `opencode-termux`)
- a working `opencode-termux` installation (`opencode` must already run)
- `curl`, `tar` and `node` (the normal opencode-termux dependencies)

One-line install:

```bash
curl -fsSL https://raw.githubusercontent.com/devnusret/opencode-turbo/main/install.sh | bash
```

Or clone the repository and run:

```bash
git clone https://github.com/devnusret/opencode-turbo.git
cd opencode-turbo
./install.sh
```

The installer:

1. downloads a verified prebuilt launcher (or builds it locally with `--from-source`),
2. installs `~/.local/bin/opencode-turbo` and an `opencode` symlink,
3. adds a small, idempotent PATH block to `~/.bashrc`,
4. patches the `opencode-termux` shell wrapper with a shim that calls the native
   launcher (the original wrapper is kept as a fallback and backed up),
5. generates the instant `--help` cache.

Open a new shell afterwards. Verify with:

```bash
opencode --version                 # should be instant
opencode-turbo --turbo-doctor      # paths, versions and a timing check
```

If the package wrapper is ever replaced by an npm update, re-apply the shim:

```bash
opencode-turbo --turbo-repair
```

The daily background update check does this automatically after it updates
opencode-termux.

## How it works

- **Native fast path.** The launcher is a small Rust binary (no runtime
  dependencies). It resolves the runtime paths once, verifies the files, sets
  the same environment as the original wrapper, and `execve`s the musl loader.
- **Instant meta commands.** `--version` is served from the installed runtime
  version file. `--help` is captured once per opencode version and replayed
  from a cache, preserving stdout/stderr.
- **Fast boot.** `OPENCODE_FAST_BOOT=1` is set when the user has not set it. This
  is an upstream opencode mode that skips its initial loading screen; data
  continues loading in the background.
- **No launch-time network.** The npm registry is checked at most once per day
  in a detached process. Failures are logged and ignored.
- **Safe fallback.** If the launcher is missing or broken, the shim continues
  into the original `opencode-termux` wrapper.

## Launcher options

These flags are consumed by `opencode-turbo` (or by `opencode` through the
shim) and are not passed to opencode:

| Option | Meaning |
| --- | --- |
| `--turbo-doctor` | Print paths, versions, shim status and a real `--version` timing. |
| `--turbo-repair` | Re-apply the wrapper shim and regenerate the help cache. |
| `--turbo-warm` | Generate the instant `--help` cache. |
| `--turbo-update` | Check and install an opencode-termux update now. |
| `--turbo-no-fast-boot` | Do not set `OPENCODE_FAST_BOOT`. |
| `--turbo-raw` | Run the real binary without fast paths or env changes. |
| `--turbo-launcher-version` | Print the launcher version. |
| `--turbo-help` | Print launcher help. |

## Environment

| Variable | Meaning |
| --- | --- |
| `OPENCODE_TURBO_FAST_BOOT=0` | Do not enable fast boot by default. |
| `OPENCODE_TURBO_NO_UPDATE_CHECK=1` | Disable the daily background update check. |
| `OPENCODE_TERMUX_AUTOUPDATE=0` | Do not auto-install newer opencode-termux builds. |
| `OPENCODE_TURBO_RUNTIME=<dir>` | Override the runtime directory (default `~/.opencode`). |
| `OPENCODE_TURBO_PACKAGE=<dir>` | Override the npm package directory. |
| `OPENCODE_TURBO_NO_INSTALL=1` | Fail instead of running the runtime installer. |

## Uninstall

```bash
./uninstall.sh
```

This restores the original `opencode-termux` wrapper from the backup, removes
the launcher and symlink, and removes the PATH block. The opencode runtime in
`~/.opencode` and your sessions are left untouched.

## Build from source

```bash
cargo build --release
./install.sh --from-source
```

The release workflow also builds a static
`aarch64-unknown-linux-musl` binary on GitHub Actions.

Benchmark helper (uses a PTY, measures first output and first full frame):

```bash
python3 scripts/bench.py --runs 3 -- opencode
python3 scripts/bench.py --runs 2 --timeout 15 -- opencode-turbo --turbo-no-fast-boot
```

## Limitations

- Android/Termux aarch64 only. `opencode-termux` itself is aarch64-only.
- The launcher does not make the Bun runtime smaller or remove its ~2 s boot
  cost. It removes wrapper overhead and the loading-screen wait.
- Fast boot shows the UI before all background data has arrived. This is the
  same trade-off as opencode's own `OPENCODE_FAST_BOOT`.
- An npm update can overwrite the shim; `--turbo-repair` fixes it. The PATH
  symlink keeps `opencode` fast in interactive shells even without the shim.
- Not affiliated with opencode, SST, or opencode-termux.

## License

MIT
