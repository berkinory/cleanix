use anyhow::{Context, Result, bail};
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn which(name: &str) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> =
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
    paths.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(home) = env::var_os("HOME") {
        for p in [".bun/bin", ".orbstack/bin", ".local/bin"] {
            paths.push(PathBuf::from(&home).join(p));
        }
    }
    paths
        .into_iter()
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}
pub fn run(executable: &Path, args: &[String], timeout: Duration) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(executable);
    cmd.args(args)
        .env("LC_ALL", "C")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_CONTEXT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Cannot start {}", executable.display()))?;
    fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
        thread::spawn(move || {
            let mut result = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = pipe.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if result.len() < 8 * 1024 * 1024 {
                    result.extend_from_slice(&buf[..n]);
                }
            }
            Ok(result)
        })
    }
    let out = drain(child.stdout.take().unwrap());
    let err = drain(child.stderr.take().unwrap());
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > timeout {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!(
                "{} timed out after {}s",
                executable.display(),
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))??;
    if !status.success() {
        bail!(
            "{}: {}",
            executable.file_name().unwrap_or_default().to_string_lossy(),
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    Ok(String::from_utf8(stdout)?.trim().to_string())
}
pub fn query(exe: &Path, args: &[&str]) -> Result<String> {
    run(
        exe,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        Duration::from_secs(8),
    )
}
pub fn json(exe: &Path, args: &[&str]) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&query(exe, args)?)?)
}

pub fn running(names: &[&str]) -> Result<bool> {
    let output = query(Path::new("/bin/ps"), &["-A", "-o", "comm="])?;
    Ok(output.lines().any(|line| {
        let name = Path::new(line.trim())
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        names
            .iter()
            .any(|n| name == *n || name.starts_with(&format!("{n}-")))
    }))
}
