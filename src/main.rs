//! opencode-turbo: a tiny native launcher for opencode-termux on Android.
//!
//! It replaces the per-launch Node/npm work of the original shell wrapper with
//! direct path resolution plus `execve` of the musl loader. The real opencode
//! binary is never modified. Auto-update checks run once per day in a detached
//! background process and never block the UI.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");
const SHIM_MARKER: &str = "# opencode-turbo shim v1";
const CHECK_INTERVAL_SECS: u64 = 86_400;
const LOCK_STALE_SECS: u64 = 3_600;
const REQUIRED_FILES: [&str; 6] = [
    "opencode",
    "ld-musl-aarch64.so.1",
    "libc.musl-aarch64.so.1",
    "libgcc_s.so.1",
    "libstdc++.so.6",
    "libstdc++.so.6.runtime",
];

#[derive(Clone)]
struct Paths {
    home: PathBuf,
    prefix: PathBuf,
    runtime: PathBuf,
    loader: PathBuf,
    binary: PathBuf,
    version_file: PathBuf,
    package: PathBuf,
    package_bin: PathBuf,
    cert: PathBuf,
    cache: PathBuf,
    backup: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Launch,
    Doctor,
    Repair,
    Warm,
    Update,
    UpdateCheck,
    LauncherHelp,
    LauncherVersion,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FastMode {
    Enable,
    Disable,
    Leave,
}

struct Options {
    action: Action,
    fast: FastMode,
    raw: bool,
}

struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("opencode-turbo: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let paths = resolve_paths()?;
    let (options, args) = parse_args()?;

    match options.action {
        Action::LauncherVersion => {
            println!("opencode-turbo {LAUNCHER_VERSION}");
            return Ok(());
        }
        Action::LauncherHelp => {
            print_launcher_help();
            return Ok(());
        }
        Action::Doctor => return doctor(&paths),
        Action::Repair => {
            ensure_runtime(&paths)?;
            let changed = apply_shim(&paths).map_err(|e| format!("could not apply shim: {e}"))?;
            warm(&paths)?;
            println!(
                "opencode-turbo: {}",
                if changed {
                    "wrapper shim applied"
                } else {
                    "wrapper shim already applied"
                }
            );
            return Ok(());
        }
        Action::Warm => {
            ensure_runtime(&paths)?;
            warm(&paths)?;
            return Ok(());
        }
        Action::Update => {
            do_update_check(&paths, true);
            return Ok(());
        }
        Action::UpdateCheck => {
            do_update_check(&paths, false);
            return Ok(());
        }
        Action::Launch => {}
    }

    if options.raw {
        return exec_real(&paths, &args, FastMode::Leave);
    }

    ensure_runtime(&paths)?;

    if args.len() == 1 && (args[0] == OsStr::new("--version") || args[0] == OsStr::new("-v")) {
        print_runtime_version(&paths);
        return Ok(());
    }

    if args.len() == 1 && (args[0] == OsStr::new("--help") || args[0] == OsStr::new("-h")) {
        return render_help(&paths, true);
    }

    maybe_spawn_update_check(&paths);
    exec_real(&paths, &args, options.fast)
}

fn parse_args() -> Result<(Options, Vec<OsString>), String> {
    let mut action = Action::Launch;
    let mut raw = false;
    let mut fast = FastMode::Enable;

    match env::var("OPENCODE_TURBO_FAST_BOOT").ok().as_deref() {
        Some("0") | Some("false") => fast = FastMode::Disable,
        _ => {}
    }

    let mut args = Vec::new();
    for arg in env::args_os().skip(1) {
        if let Some(text) = arg.to_str() {
            match text {
                "--turbo-doctor" => {
                    action = Action::Doctor;
                    continue;
                }
                "--turbo-repair" => {
                    action = Action::Repair;
                    continue;
                }
                "--turbo-warm" => {
                    action = Action::Warm;
                    continue;
                }
                "--turbo-update" => {
                    action = Action::Update;
                    continue;
                }
                "--turbo-update-check" => {
                    action = Action::UpdateCheck;
                    continue;
                }
                "--turbo-help" => {
                    action = Action::LauncherHelp;
                    continue;
                }
                "--turbo-launcher-version" => {
                    action = Action::LauncherVersion;
                    continue;
                }
                "--turbo-no-fast-boot" => {
                    fast = FastMode::Disable;
                    continue;
                }
                "--turbo-raw" => {
                    raw = true;
                    fast = FastMode::Leave;
                    continue;
                }
                _ => {}
            }
        }
        args.push(arg);
    }

    if raw {
        fast = FastMode::Leave;
    }

    Ok((Options { action, fast, raw }, args))
}

fn resolve_paths() -> Result<Paths, String> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())?;
    let prefix = env::var_os("PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/data/com.termux/files/usr"));

    let runtime = env::var_os("OPENCODE_TURBO_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".opencode"));

    let package = resolve_package(&prefix)?;
    let package_bin = package.join("bin").join("opencode");
    let cache = home.join(".cache").join("opencode-turbo");
    let backup = home
        .join(".local")
        .join("share")
        .join("opencode-turbo")
        .join("opencode-wrapper.orig");

    Ok(Paths {
        loader: runtime.join("ld-musl-aarch64.so.1"),
        binary: runtime.join("opencode"),
        version_file: runtime.join(".opencode-termux-version"),
        cert: prefix.join("etc").join("tls").join("cert.pem"),
        home,
        prefix,
        runtime,
        package,
        package_bin,
        cache,
        backup,
    })
}

fn resolve_package(prefix: &Path) -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("OPENCODE_TURBO_PACKAGE") {
        let path = PathBuf::from(path);
        if is_package_dir(&path) {
            return Ok(path);
        }
    }

    let direct = prefix
        .join("lib")
        .join("node_modules")
        .join("opencode-termux");
    if is_package_dir(&direct) {
        return Ok(direct);
    }

    if let Ok(target) = fs::canonicalize(prefix.join("bin").join("opencode")) {
        if target.file_name() == Some(OsStr::new("opencode")) {
            if let Some(bin_dir) = target.parent() {
                if let Some(package) = bin_dir.parent() {
                    if is_package_dir(package) {
                        return Ok(package.to_path_buf());
                    }
                }
            }
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("opencode");
            if let Ok(target) = fs::canonicalize(&candidate) {
                if let Some(bin_dir) = target.parent() {
                    if let Some(package) = bin_dir.parent() {
                        if is_package_dir(package) {
                            return Ok(package.to_path_buf());
                        }
                    }
                }
            }
        }
    }

    for candidate in [
        PathBuf::from("/usr/local/lib/node_modules/opencode-termux"),
        PathBuf::from("/usr/lib/node_modules/opencode-termux"),
    ] {
        if is_package_dir(&candidate) {
            return Ok(candidate);
        }
    }

    Err("could not find the opencode-termux package (set OPENCODE_TURBO_PACKAGE)".to_string())
}

fn is_package_dir(path: &Path) -> bool {
    path.join("package.json").is_file()
        && path.join("install.js").is_file()
        && path.join("bin").join("opencode").is_file()
}

fn runtime_ok(paths: &Paths) -> bool {
    for name in REQUIRED_FILES {
        if !file_nonempty(&paths.runtime.join(name)) {
            return false;
        }
    }

    match package_version(paths) {
        Some(expected) => runtime_version(paths).as_deref() == Some(expected.as_str()),
        None => true,
    }
}

fn file_nonempty(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn ensure_runtime(paths: &Paths) -> Result<(), String> {
    if runtime_ok(paths) {
        return Ok(());
    }

    if env::var("OPENCODE_TURBO_NO_INSTALL").ok().as_deref() == Some("1") {
        return Err(format!(
            "runtime in {} is missing or outdated and OPENCODE_TURBO_NO_INSTALL=1",
            paths.runtime.display()
        ));
    }

    eprintln!("opencode-turbo: preparing opencode runtime (first run or version change)...");
    let node = find_program("node", &paths.prefix)
        .ok_or_else(|| "node was not found; opencode-termux needs Node.js".to_string())?;
    let install_js = paths.package.join("install.js");
    let status = Command::new(&node)
        .arg(&install_js)
        .current_dir(&paths.package)
        .status()
        .map_err(|e| format!("could not start {}: {e}", node.display()))?;

    if !status.success() {
        return Err(format!("runtime installer failed with status {status}"));
    }
    if !runtime_ok(paths) {
        return Err(format!(
            "runtime in {} is still incomplete after installation",
            paths.runtime.display()
        ));
    }

    let _ = apply_shim(paths);
    Ok(())
}

fn package_version(paths: &Paths) -> Option<String> {
    let text = fs::read_to_string(paths.package.join("package.json")).ok()?;
    json_string_field(&text, "version")
}

fn runtime_version(paths: &Paths) -> Option<String> {
    let text = fs::read_to_string(&paths.version_file).ok()?;
    let version = text.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn effective_version(paths: &Paths) -> String {
    runtime_version(paths)
        .or_else(|| package_version(paths))
        .unwrap_or_else(|| "unknown".to_string())
}

fn print_runtime_version(paths: &Paths) {
    println!("{}", effective_version(paths));
}

fn base_command(paths: &Paths, args: &[OsString]) -> Command {
    let mut command = Command::new(&paths.loader);
    command
        .arg("--library-path")
        .arg(&paths.runtime)
        .arg(&paths.binary)
        .args(args)
        .env("SSL_CERT_FILE", &paths.cert)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .env_remove("LD_PRELOAD");
    command
}

fn exec_real(paths: &Paths, args: &[OsString], fast: FastMode) -> Result<(), String> {
    let mut command = base_command(paths, args);
    match fast {
        FastMode::Enable => {
            if env::var_os("OPENCODE_FAST_BOOT").is_none() {
                command.env("OPENCODE_FAST_BOOT", "1");
            }
        }
        FastMode::Disable => {
            command.env_remove("OPENCODE_FAST_BOOT");
        }
        FastMode::Leave => {}
    }

    let error = command.exec();
    Err(format!(
        "could not exec {}: {error} (try --turbo-doctor)",
        paths.loader.display()
    ))
}

fn help_cache_paths(paths: &Paths) -> (PathBuf, PathBuf) {
    let version = effective_version(paths);
    let safe: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stdout = paths.cache.join(format!("help-{safe}.out"));
    let stderr = paths.cache.join(format!("help-{safe}.err"));
    (stdout, stderr)
}

fn read_cache_file(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

fn render_help(paths: &Paths, emit: bool) -> Result<(), String> {
    let (cache_out, cache_err) = help_cache_paths(paths);
    let cached_out = read_cache_file(&cache_out);
    let cached_err = read_cache_file(&cache_err);
    if !cached_out.is_empty() || !cached_err.is_empty() {
        if emit {
            write_streams(&cached_out, &cached_err)?;
        }
        return Ok(());
    }

    let output = base_command(paths, &[OsString::from("--help")])
        .output()
        .map_err(|e| format!("could not start opencode for --help: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "opencode --help failed with status {}",
            output.status
        ));
    }
    if output.stdout.is_empty() && output.stderr.is_empty() {
        return Err("opencode --help produced no output".to_string());
    }

    if let Err(error) = fs::create_dir_all(&paths.cache) {
        eprintln!(
            "opencode-turbo: warning: could not create {}: {error}",
            paths.cache.display()
        );
    } else {
        if let Err(error) = fs::write(&cache_out, &output.stdout) {
            eprintln!(
                "opencode-turbo: warning: could not cache help stdout in {}: {error}",
                cache_out.display()
            );
        }
        if let Err(error) = fs::write(&cache_err, &output.stderr) {
            eprintln!(
                "opencode-turbo: warning: could not cache help stderr in {}: {error}",
                cache_err.display()
            );
        }
    }

    if emit {
        write_streams(&output.stdout, &output.stderr)?;
    }
    Ok(())
}

fn write_streams(stdout_bytes: &[u8], stderr_bytes: &[u8]) -> Result<(), String> {
    if !stdout_bytes.is_empty() {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(stdout_bytes)
            .and_then(|_| stdout.flush())
            .map_err(|e| format!("could not write help output: {e}"))?;
    }
    if !stderr_bytes.is_empty() {
        let mut stderr = io::stderr().lock();
        stderr
            .write_all(stderr_bytes)
            .and_then(|_| stderr.flush())
            .map_err(|e| format!("could not write help output: {e}"))?;
    }
    Ok(())
}

fn warm(paths: &Paths) -> Result<(), String> {
    render_help(paths, false)?;
    let (cache_out, cache_err) = help_cache_paths(paths);
    println!(
        "opencode-turbo: help cache ready for opencode {} ({} + {})",
        effective_version(paths),
        cache_out.display(),
        cache_err.display()
    );
    Ok(())
}

fn apply_shim(paths: &Paths) -> io::Result<bool> {
    let content = fs::read_to_string(&paths.package_bin)?;
    if content.contains(SHIM_MARKER) {
        return Ok(false);
    }

    if let Some(parent) = paths.backup.parent() {
        fs::create_dir_all(parent)?;
    }
    if !paths.backup.exists() {
        fs::copy(&paths.package_bin, &paths.backup)?;
    }

    let body = content
        .split_once('\n')
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let shim = format!(
        "#!/data/data/com.termux/files/usr/bin/bash\n{SHIM_MARKER}\n\
         # Fast path. If the launcher is absent, the original wrapper below runs.\n\
         if [ -x \"$HOME/.local/bin/opencode-turbo\" ]; then\n\
         \x20 exec \"$HOME/.local/bin/opencode-turbo\" \"$@\"\n\
         fi\n\
         # --- original opencode-termux wrapper follows ---\n{body}"
    );

    let mode = fs::metadata(&paths.package_bin)?.permissions().mode();
    let temp = paths
        .package_bin
        .with_file_name(format!(".opencode-turbo-shim.{}.tmp", process::id()));
    fs::write(&temp, shim)?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    if let Err(error) = fs::rename(&temp, &paths.package_bin) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(true)
}

fn doctor(paths: &Paths) -> Result<(), String> {
    println!("opencode-turbo {LAUNCHER_VERSION}");
    println!(
        "launcher         {}",
        env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    );
    println!(
        "package          {} (version {})",
        paths.package.display(),
        package_version(paths).unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "runtime          {} (version {})",
        paths.runtime.display(),
        runtime_version(paths).unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "runtime ready    {}",
        if runtime_ok(paths) { "yes" } else { "NO" }
    );
    println!("loader           {}", status_line(&paths.loader, true));
    println!("binary           {}", status_line(&paths.binary, true));
    println!("certificate      {}", status_line(&paths.cert, false));

    let shimmed = fs::read_to_string(&paths.package_bin)
        .map(|text| text.contains(SHIM_MARKER))
        .unwrap_or(false);
    println!(
        "wrapper shim     {}",
        if shimmed {
            "applied"
        } else {
            "NOT applied (run: opencode-turbo --turbo-repair)"
        }
    );
    println!("wrapper backup   {}", status_line(&paths.backup, false));
    println!(
        "launcher link    {}",
        status_line(
            &paths.home.join(".local").join("bin").join("opencode-turbo"),
            false
        )
    );

    if let Some((_, path)) = find_in_path("opencode") {
        println!("PATH opencode    {}", path.display());
    } else {
        println!("PATH opencode    not found");
    }

    let (last_check, latest) = read_update_state(paths);
    let now = now_secs();
    println!(
        "update state     latest={} checked={}",
        latest.unwrap_or_else(|| "unknown".to_string()),
        if last_check == 0 {
            "never".to_string()
        } else {
            format!("{}s ago", now.saturating_sub(last_check))
        }
    );
    println!(
        "fast boot        {}",
        env::var("OPENCODE_FAST_BOOT").unwrap_or_else(|_| "unset (launcher sets 1)".to_string())
    );

    if file_nonempty(&paths.loader) && file_nonempty(&paths.binary) {
        let started = Instant::now();
        match base_command(paths, &[OsString::from("--version")]).output() {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                println!(
                    "real --version   {} ({:.0} ms)",
                    version.trim(),
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Ok(output) => println!("real --version   failed ({})", output.status),
            Err(error) => println!("real --version   failed ({error})"),
        }
    }

    Ok(())
}

fn status_line(path: &Path, must_be_nonempty: bool) -> String {
    match fs::metadata(path) {
        Ok(metadata) => {
            let ok = !must_be_nonempty || metadata.len() > 0;
            format!(
                "{} ({} bytes, {})",
                path.display(),
                metadata.len(),
                if ok { "ok" } else { "empty" }
            )
        }
        Err(error) => format!("{} ({error})", path.display()),
    }
}

fn print_launcher_help() {
    println!(
        "opencode-turbo {LAUNCHER_VERSION}\n\
         Fast native launcher for opencode-termux (Android/Termux).\n\n\
         Usage:\n\
         \x20 opencode [opencode args...]        normal launch (via the wrapper shim)\n\
         \x20 opencode-turbo [opencode args...]  normal launch directly\n\n\
         Launcher options (removed before opencode sees the arguments):\n\
         \x20 --turbo-doctor              show paths, versions and timing\n\
         \x20 --turbo-repair              re-apply the wrapper shim and warm caches\n\
         \x20 --turbo-warm                pre-generate the instant --help cache\n\
         \x20 --turbo-update              check and install opencode-termux updates now\n\
         \x20 --turbo-no-fast-boot        do not set OPENCODE_FAST_BOOT\n\
         \x20 --turbo-raw                 run the real binary without launcher fast paths\n\
         \x20 --turbo-launcher-version    print the launcher version\n\
         \x20 --turbo-help                show this help\n\n\
         Environment:\n\
         \x20 OPENCODE_TURBO_FAST_BOOT=0   disable automatic OPENCODE_FAST_BOOT=1\n\
         \x20 OPENCODE_TURBO_NO_UPDATE_CHECK=1  disable the daily background update check\n\
         \x20 OPENCODE_TERMUX_AUTOUPDATE=0      disable background npm updates\n\
         \x20 OPENCODE_TURBO_RUNTIME=<dir>      override the runtime directory\n\
         \x20 OPENCODE_TURBO_PACKAGE=<dir>      override the npm package directory"
    );
}

fn maybe_spawn_update_check(paths: &Paths) {
    if env::var_os("OPENCODE_TURBO_BACKGROUND").is_some() {
        return;
    }
    if env::var_os("OPENCODE_TURBO_NO_UPDATE_CHECK").is_some() {
        return;
    }
    if env::var("OPENCODE_TERMUX_AUTOUPDATE").ok().as_deref() == Some("0") {
        return;
    }

    let (last_check, _) = read_update_state(paths);
    if now_secs() < last_check.saturating_add(CHECK_INTERVAL_SECS) {
        return;
    }

    if fs::create_dir_all(&paths.cache).is_err() {
        return;
    }
    let log_path = paths.cache.join("update.log");
    let Ok(log) = OpenOptions::new().create(true).append(true).open(&log_path) else {
        return;
    };
    let Ok(err) = log.try_clone() else {
        return;
    };

    let executable = env::current_exe()
        .unwrap_or_else(|_| paths.home.join(".local").join("bin").join("opencode-turbo"));

    let mut command = Command::new(executable);
    command
        .arg("--turbo-update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .env("OPENCODE_TURBO_BACKGROUND", "1");
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let _ = command.spawn();
}

fn do_update_check(paths: &Paths, verbose: bool) {
    let Some(_lock) = acquire_lock(paths, verbose) else {
        return;
    };

    note(paths, verbose, "checking npm for a newer opencode-termux");
    let (_, previous_latest) = read_update_state(paths);
    let current = package_version(paths)
        .or_else(|| runtime_version(paths))
        .unwrap_or_default();

    let latest = fetch_latest_version(paths);
    let now = now_secs();

    let Some(latest) = latest else {
        write_update_state(paths, now, previous_latest.as_deref(), None);
        note(
            paths,
            verbose,
            "update check failed (offline or registry error)",
        );
        return;
    };

    write_update_state(paths, now, Some(&latest), None);

    if current.is_empty() {
        note(
            paths,
            verbose,
            "current version is unknown; not updating automatically",
        );
        return;
    }

    if !is_newer(&latest, &current) {
        note(
            paths,
            verbose,
            &format!("opencode-termux is up to date ({current})"),
        );
        return;
    }

    if !auto_update_enabled() {
        note(
            paths,
            verbose,
            &format!("update {latest} is available (auto-update disabled)"),
        );
        return;
    }

    note(
        paths,
        verbose,
        &format!("updating opencode-termux {current} -> {latest}"),
    );
    if install_update(paths, &latest, verbose) {
        write_update_state(paths, now, Some(&latest), Some(&latest));
    }
}

fn auto_update_enabled() -> bool {
    env::var("OPENCODE_TERMUX_AUTOUPDATE").ok().as_deref() != Some("0")
}

fn install_update(paths: &Paths, latest: &str, verbose: bool) -> bool {
    let Some(npm) = find_program("npm", &paths.prefix) else {
        note(paths, verbose, "npm was not found; cannot update");
        return false;
    };

    let package_spec = format!("opencode-termux@{latest}");
    let status = run_with_timeout(
        &["600"],
        &npm,
        &[
            OsStr::new("install"),
            OsStr::new("-g"),
            OsStr::new(&package_spec),
            OsStr::new("--ignore-scripts"),
            OsStr::new("--silent"),
        ],
        None,
    );

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            note(
                paths,
                verbose,
                &format!("npm install failed with status {status}"),
            );
            return false;
        }
        Err(error) => {
            note(paths, verbose, &format!("could not run npm: {error}"));
            return false;
        }
    }

    let Some(node) = find_program("node", &paths.prefix) else {
        note(
            paths,
            verbose,
            "node was not found; runtime was not updated",
        );
        return false;
    };
    let install_js = paths.package.join("install.js");
    let status = run_with_timeout(
        &["900"],
        &node,
        &[install_js.as_os_str()],
        Some(&paths.package),
    );

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            note(
                paths,
                verbose,
                &format!("runtime installer failed with status {status}"),
            );
            return false;
        }
        Err(error) => {
            note(
                paths,
                verbose,
                &format!("could not run runtime installer: {error}"),
            );
            return false;
        }
    }

    let refreshed_package =
        resolve_package(&paths.prefix).unwrap_or_else(|_| paths.package.clone());
    let refreshed = Paths {
        package: refreshed_package.clone(),
        package_bin: refreshed_package.join("bin").join("opencode"),
        ..paths.clone()
    };
    let _ = apply_shim(&refreshed);

    let executable = env::current_exe()
        .unwrap_or_else(|_| paths.home.join(".local").join("bin").join("opencode-turbo"));
    let _ = Command::new(executable)
        .arg("--turbo-warm")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    note(paths, verbose, &format!("updated to {latest}"));
    true
}

fn run_with_timeout(
    timeout_args: &[&str],
    program: &Path,
    args: &[&OsStr],
    cwd: Option<&Path>,
) -> io::Result<process::ExitStatus> {
    let timeout = find_in_path("timeout").map(|(_, path)| path);
    let mut command = match timeout {
        Some(timeout) => {
            let mut command = Command::new(timeout);
            command.args(timeout_args).arg(program).args(args);
            command
        }
        None => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
    };
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.status()
}

fn fetch_latest_version(paths: &Paths) -> Option<String> {
    let curl = find_program("curl", &paths.prefix)?;
    let timeout = find_in_path("timeout").map(|(_, path)| path);

    let mut command = match timeout {
        Some(timeout) => {
            let mut command = Command::new(timeout);
            command.args(["40"]).arg(&curl);
            command
        }
        None => Command::new(&curl),
    };
    let output = command
        .args([
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            "Accept: application/json",
            "https://registry.npmjs.org/opencode-termux/latest",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    json_string_field(&text, "version")
}

fn acquire_lock(paths: &Paths, verbose: bool) -> Option<Lock> {
    if fs::create_dir_all(&paths.cache).is_err() {
        return None;
    }
    let lock_path = paths.cache.join("update.lock");

    for attempt in 0..2 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "pid={} time={}", process::id(), now_secs());
                return Some(Lock(lock_path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&lock_path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .map(|age| age.as_secs() > LOCK_STALE_SECS)
                    .unwrap_or(false);
                if stale && attempt == 0 {
                    let _ = fs::remove_file(&lock_path);
                    continue;
                }
                note(paths, verbose, "another update check is already running");
                return None;
            }
            Err(error) => {
                note(
                    paths,
                    verbose,
                    &format!("could not create update lock: {error}"),
                );
                return None;
            }
        }
    }
    None
}

fn read_update_state(paths: &Paths) -> (u64, Option<String>) {
    let text = fs::read_to_string(paths.cache.join("update-state.txt")).unwrap_or_default();
    let mut last_check = 0u64;
    let mut latest = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("last_check=") {
            last_check = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("latest=") {
            let value = value.trim();
            if !value.is_empty() {
                latest = Some(value.to_string());
            }
        }
    }
    (last_check, latest)
}

fn write_update_state(paths: &Paths, last_check: u64, latest: Option<&str>, applied: Option<&str>) {
    if fs::create_dir_all(&paths.cache).is_err() {
        return;
    }
    let mut text = format!("last_check={last_check}\n");
    if let Some(latest) = latest {
        text.push_str(&format!("latest={latest}\n"));
    }
    if let Some(applied) = applied {
        text.push_str(&format!("applied={applied}\n"));
        text.push_str(&format!("applied_at={}\n", now_secs()));
    }
    let state_path = paths.cache.join("update-state.txt");
    let temp_path = paths.cache.join("update-state.txt.tmp");
    if fs::write(&temp_path, text).is_ok() {
        let _ = fs::rename(&temp_path, &state_path);
    }
}

fn note(paths: &Paths, verbose: bool, message: &str) {
    let line = format!("opencode-turbo: {message}");
    if verbose {
        println!("{line}");
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.cache.join("update.log"))
    {
        let _ = writeln!(file, "{} {line}", now_secs());
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn find_program(name: &str, prefix: &Path) -> Option<PathBuf> {
    find_in_path(name).map(|(_, path)| path).or_else(|| {
        let candidate = prefix.join("bin").join(name);
        if is_executable(&candidate) {
            Some(candidate)
        } else {
            None
        }
    })
}

fn find_in_path(name: &str) -> Option<(PathBuf, PathBuf)> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some((dir, candidate));
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn json_string_field(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let index = text.find(&pattern)?;
    let rest = &text[index + pattern.len()..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let quote = rest.find('"')?;
    let rest = &rest[quote + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_version(input: &str) -> Option<(Vec<u64>, bool)> {
    let input = input.trim().trim_start_matches('v');
    let core = input.split(['-', '+']).next()?;
    if core.is_empty() {
        return None;
    }
    let mut numbers = Vec::new();
    for part in core.split('.') {
        numbers.push(part.parse::<u64>().ok()?);
    }
    let stable = !input.contains('-');
    Some((numbers, stable))
}

fn is_newer(latest: &str, current: &str) -> bool {
    let Some((latest_numbers, latest_stable)) = parse_version(latest) else {
        return false;
    };
    let Some((current_numbers, current_stable)) = parse_version(current) else {
        return false;
    };

    let length = latest_numbers.len().max(current_numbers.len());
    for index in 0..length {
        let a = latest_numbers.get(index).copied().unwrap_or(0);
        let b = current_numbers.get(index).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    latest_stable && !current_stable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_field_reads_first_value() {
        let text = r#"{"name":"opencode-termux","version":"1.19.3","x":"version"}"#;
        assert_eq!(
            json_string_field(text, "version").as_deref(),
            Some("1.19.3")
        );
    }

    #[test]
    fn version_compare_basic() {
        assert!(is_newer("1.19.4", "1.19.3"));
        assert!(is_newer("1.20.0", "1.19.9"));
        assert!(!is_newer("1.19.3", "1.19.3"));
        assert!(!is_newer("1.19.2", "1.19.3"));
        assert!(is_newer("1.19.3", "1.19.3-beta.1"));
    }

    #[test]
    fn version_parse_rejects_garbage() {
        assert!(parse_version("").is_none());
        assert!(parse_version("nightly").is_none());
        assert_eq!(parse_version("v1.2.3").unwrap().0, vec![1, 2, 3]);
    }
}
