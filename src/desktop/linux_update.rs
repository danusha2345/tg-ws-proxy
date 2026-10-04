//! Установка проверенного обновления Linux и перезапуск после выхода приложения.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Deb,
    Rpm,
    Binary,
    Unsupported,
}

fn owns(program: &str, arguments: &[&str], exe: &Path) -> bool {
    Command::new(program)
        .args(arguments)
        .arg(exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn detect(exe: &Path) -> Kind {
    if owns("pacman", &["-Qo"], exe) {
        return Kind::Unsupported;
    }
    if owns("dpkg-query", &["-S"], exe) {
        return Kind::Deb;
    }
    if owns("rpm", &["-qf"], exe) {
        return Kind::Rpm;
    }
    // Не заменяем неизвестные системные установки или сборку разработчика.
    if exe.starts_with("/usr")
        || exe.starts_with("/opt")
        || exe.components().any(|p| p.as_os_str() == "target")
    {
        return Kind::Unsupported;
    }
    Kind::Binary
}

pub(super) fn asset(kind: Kind, arch: &str) -> Option<&'static str> {
    match (kind, arch) {
        (Kind::Deb, "amd64") => Some("TgWsProxy_linux_amd64.deb"),
        (Kind::Deb, "arm64") => Some("TgWsProxy_linux_arm64.deb"),
        (Kind::Rpm, "amd64") => Some("TgWsProxy_linux_amd64.rpm"),
        (Kind::Rpm, "arm64") => Some("TgWsProxy_linux_arm64.rpm"),
        (_, "amd64") => Some("TgWsProxy_linux_amd64"),
        (_, "arm64") => Some("TgWsProxy_linux_arm64"),
        _ => None,
    }
}

pub(super) fn replace_binary(source: &Path, target: &Path) -> Result<()> {
    let directory = target.parent().context("нет каталога приложения")?;
    let mut temporary = NamedTempFile::new_in(directory)?;
    let mut input = fs::File::open(source)?;
    std::io::copy(&mut input, temporary.as_file_mut())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))?;
    temporary.as_file().sync_all()?;
    temporary.persist(target).map_err(|error| error.error)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) async fn install(path: PathBuf) -> Result<bool> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let kind = detect(&exe);
    if kind == Kind::Unsupported {
        open::that("https://github.com/danusha2345/tg-ws-proxy/releases")?;
        return Ok(false);
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => bail!("архитектура не поддерживается"),
    };
    if path.file_name().and_then(|name| name.to_str()) != asset(kind, arch) {
        bail!("способ установки изменился после скачивания; скачайте обновление заново");
    }
    if kind == Kind::Binary {
        let target = exe.clone();
        tokio::task::spawn_blocking(move || replace_binary(&path, &target)).await??;
    } else {
        let mut command = tokio::process::Command::new("pkexec");
        match kind {
            Kind::Deb => {
                command.args(["/usr/bin/dpkg", "-i"]);
            }
            Kind::Rpm => {
                command.args(["/usr/bin/rpm", "-U"]);
            }
            _ => unreachable!(),
        }
        command.arg(&path).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(600), command.output())
            .await
            .context("время установки истекло")??;
        if !output.status.success() {
            bail!(
                "установка обновления завершилась с {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    restart_after_exit(&exe)?;
    Ok(true)
}

fn restart_command(exe: &Path, pid: u32) -> Command {
    // Пути и аргументы передаются отдельно и не интерпретируются как shell-код.
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("pid=$1; shift; n=0; while kill -0 \"$pid\" 2>/dev/null; do n=$((n+1)); [ \"$n\" -lt 120 ] || exit 1; sleep 1; done; exec \"$@\"")
        .arg("tg-ws-proxy-restart")
        .arg(pid.to_string()).arg(exe)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

fn restart_after_exit(exe: &Path) -> Result<()> {
    restart_command(exe, std::process::id())
        .args(std::env::args_os().skip(1))
        .spawn()
        .context("не удалось запланировать перезапуск")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_preserves_running_inode_and_makes_new_binary_executable() {
        use std::io::Read;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("new binary");
        let target = directory.path().join("running binary");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();
        let mut running = fs::File::open(&target).unwrap();
        replace_binary(&source, &target).unwrap();
        let mut old = String::new();
        running.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old");
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[test]
    fn missing_source_preserves_installed_binary() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("app");
        fs::write(&target, b"old").unwrap();
        assert!(replace_binary(&directory.path().join("missing"), &target).is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }
    #[test]
    fn restart_waits_for_old_process_and_preserves_literal_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let exe = directory.path().join("app with ' quote");
        let marker = directory.path().join("marker");
        fs::write(&exe, "#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let mut old = Command::new("sleep").arg("30").spawn().unwrap();
        let literal = "$(touch SHOULD_NOT_EXIST); 'quoted'";
        let mut helper = restart_command(&exe, old.id())
            .arg(&marker)
            .arg(literal)
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists());
        old.kill().unwrap();
        old.wait().unwrap();
        assert!(helper.wait().unwrap().success());
        assert_eq!(fs::read_to_string(marker).unwrap(), literal);
    }

    #[test]
    fn architecture_and_installation_select_matching_asset() {
        assert_eq!(asset(Kind::Rpm, "arm64"), Some("TgWsProxy_linux_arm64.rpm"));
        assert_eq!(asset(Kind::Binary, "amd64"), Some("TgWsProxy_linux_amd64"));
        assert_eq!(asset(Kind::Deb, "unknown"), None);
    }
}
