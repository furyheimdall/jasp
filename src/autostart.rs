use anyhow::{Context, Result, bail, ensure};
use std::{env, fs, io::{self, Write}, os::unix::{fs::PermissionsExt, process::ExitStatusExt}, path::{Path, PathBuf}, process::Command};

const MARKER: &str = "# Jasp startup preference v3";
const OLD_MARKER: &str = "# Jasp startup preference v2";
const VENDOR_BRANCH: &str = "if [ $WEBUI_SHOW = \"1\" ]; then\n";
const OLD_WEB: &str = concat!("# Jasp startup preference v2\n", "if [ \"$WEBUI_SHOW\" = \"1\" ]; then\n", "\tSTARTUP_PLAYER=$(cat -- \"$HOME/.config/jasp/startup-player\" 2>/dev/null) || STARTUP_PLAYER=native\n", "\tif [ \"$STARTUP_PLAYER\" = \"web\" ]; then\n");
const OLD_LAUNCH: &str = "\tfi\n\texec env JASP_MOODE_KIOSK=1 \"$HOME/.local/bin/jasp\" --server http://127.0.0.1 --fullscreen\n";
const NATIVE_HOOK: &str = concat!("\t# Jasp startup preference v3\n", "\tif [ -x /usr/bin/jasp ] && [ \"$(cat -- \"$HOME/.config/jasp/startup-player\" 2>/dev/null)\" = native ]; then\n", "\t\texec env JASP_MOODE_KIOSK=1 /usr/bin/jasp --server http://127.0.0.1 --fullscreen\n", "\tfi\n");
const DESKTOP_ENTRY: &str = "# Managed by Jasp\n[Desktop Entry]\nType=Application\nName=Jasp\nExec=/usr/bin/jasp\nTryExec=/usr/bin/jasp\nTerminal=false\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend { Moode, Desktop, Lite }
impl Backend {
    fn name(self) -> &'static str {
        match self { Self::Moode => "moode", Self::Desktop => "desktop", Self::Lite => "lite" }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Action { Enable, Disable, Status }

pub(crate) struct Autostart {
    home: PathBuf,
    config: PathBuf,
    pub(crate) backend: Backend,
}

pub(crate) fn installed() -> bool { Path::new("/usr/bin/jasp").is_file() }

pub(crate) fn command(action: Action) -> Result<()> {
    let setup = Autostart::detect()?;
    match action {
        Action::Enable => setup.set(true, true)?,
        Action::Disable => setup.set(false, true)?,
        Action::Status => {}
    }
    println!("{}: {}", setup.backend.name(), if setup.enabled()? { "enabled" } else { "disabled" });
    Ok(())
}

impl Autostart {
    pub(crate) fn detect() -> Result<Self> {
        let absolute = |key| env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute());
        let home = absolute("HOME").context("HOME must be an absolute path")?;
        let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let backend = if env::var("JASP_MOODE_KIOSK").as_deref() == Ok("1") {
            Backend::Moode
        } else if env::var("JASP_LITE_KIOSK").as_deref() == Ok("1") {
            Backend::Lite
        } else if env::var_os("XDG_CURRENT_DESKTOP").is_some_and(|name| !name.is_empty()) || Path::new("/etc/systemd/system/display-manager.service").exists() {
            Backend::Desktop
        } else if moode_utility().is_some() {
            Backend::Moode
        } else if ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|key| env::var_os(key).is_some_and(|v| !v.is_empty())) {
            Backend::Desktop
        } else {
            Backend::Lite
        };
        Ok(Self { home, config, backend })
    }

    fn preference(&self) -> PathBuf { self.home.join(".config/jasp/startup-player") }
    fn launcher(&self) -> PathBuf { self.home.join(".xinitrc") }
    fn desktop(&self) -> PathBuf { self.config.join("autostart/jasp.desktop") }

    pub(crate) fn enabled(&self) -> Result<bool> {
        match self.backend {
            Backend::Moode => {
                let Some(script) = read_regular(&self.launcher())? else { return Ok(false) };
                let choice = read_regular(&self.preference())?;
                if script.lines().any(|line| line.trim() == MARKER) {
                    Ok(choice.is_some_and(|value| value.trim() == "native"))
                } else if script.lines().any(|line| line == OLD_MARKER) {
                    Ok(choice.is_none_or(|value| value.trim() != "web"))
                } else { Ok(false) }
            }
            Backend::Desktop => Ok(read_regular(&self.desktop())?.as_deref().is_some_and(desktop_managed)),
            Backend::Lite => {
                let status = Command::new("systemctl").args(["is-enabled", "--quiet", &unit()?]).status()?;
                ensure!(status.code().is_some_and(|code| code == 0 || code == 1 || code == 4), "Could not read the kiosk unit status");
                Ok(status.success())
            }
        }
    }

    pub(crate) fn set(&self, on: bool, terminal: bool) -> Result<()> {
        ensure!(!on || installed(), "Install the Jasp Debian package before enabling autostart");
        let user = username()?;
        ensure!(user != "root", "Run this as the user who will use Jasp, not as root");
        match self.backend {
            Backend::Moode => {
                let display_user = text_output(Command::new("systemctl").args(["show", "localdisplay.service", "--property=User", "--value"]))?;
                ensure!(display_user.trim() == user, "Run this as the moOde local display user ({})", display_user.trim());
                let xinitrc = self.launcher();
                let script = read_regular(&xinitrc)?.context("moOde's .xinitrc is missing")?;
                if on {
                    let utility = moode_utility().context("moOde's local display utility is unavailable")?;
                    let state = text_output(Command::new(utility).args(["-q", "SELECT value FROM cfg_system WHERE param='local_display'"]))?;
                    ensure!(state.trim() == "1", "Enable Local Display in moOde settings first");
                    if let Some(new) = add_moode_hook(&script)? {
                        let backup = self.home.join(".xinitrc.pre-jasp");
                        let mode = fs::metadata(&xinitrc)?.permissions().mode() & 0o777;
                        if read_regular(&backup)?.is_none() { write_file(&backup, &script, mode)?; }
                        checked_xinitrc(&xinitrc, &new, mode)?;
                    }
                }
                if on || script.lines().any(|line| matches!(line.trim(), MARKER | OLD_MARKER)) {
                    write_file(&self.preference(), if on { "native\n" } else { "web\n" }, 0o644)?;
                }
            }
            Backend::Desktop => {
                let path = self.desktop();
                let current = read_regular(&path)?;
                ensure!(current.as_deref().is_none_or(desktop_managed), "{} belongs to a different autostart configuration", path.display());
                if on { write_file(&path, DESKTOP_ENTRY, 0o644)?; }
                else if current.is_some() { fs::remove_file(path)?; }
            }
            Backend::Lite => {
                ensure!(terminal, "From a terminal, run jasp autostart enable or jasp autostart disable");
                if on {
                    ensure!(Path::new("/usr/bin/cage").is_file(), "Install the cage and libpam-systemd packages first");
                    ensure!(moode_utility().is_none(), "Use the moOde Local Display integration instead of the Lite service");
                }
                let status = Command::new("sudo").args(["systemctl", if on { "enable" } else { "disable" }, &unit()?]).status()?;
                ensure!(status.success(), "systemctl did not change the kiosk unit");
            }
        }
        Ok(())
    }
}

fn desktop_managed(contents: &str) -> bool {
    contents == DESKTOP_ENTRY || contents == include_str!("../packaging/jasp.desktop")
}

fn add_moode_hook(script: &str) -> Result<Option<String>> {
    if script.lines().any(|line| line.trim() == MARKER) {
        ensure!(script.matches(NATIVE_HOOK).count() == 1, "Existing Jasp launch hook was modified; refusing to overwrite it");
        return Ok(None);
    }
    let mut hook = String::from(VENDOR_BRANCH);
    hook.push_str(NATIVE_HOOK);
    if script.lines().any(|line| line == OLD_MARKER) {
        ensure!(script.matches(OLD_WEB).count() == 1 && script.matches(OLD_LAUNCH).count() == 1,
            "Existing Jasp launcher is not a recognized version; refusing to overwrite it");
        return Ok(Some(script.replacen(OLD_WEB, &hook, 1).replacen(OLD_LAUNCH, "", 1)));
    }
    ensure!(script.matches(VENDOR_BRANCH).count() == 1 && script.contains("/var/www/"),
        "Unrecognized moOde .xinitrc; refusing to overwrite it");
    Ok(Some(script.replacen(VENDOR_BRANCH, &hook, 1)))
}

fn moode_utility() -> Option<&'static str> {
    ["/usr/local/bin/moodeutl", "/usr/bin/moodeutl"].into_iter().find(|path| Path::new(path).is_file())
}
fn text_output(command: &mut Command) -> Result<String> {
    let output = command.output().with_context(|| format!("Could not run {}", command.get_program().to_string_lossy()))?;
    if !output.status.success() {
        bail!("{} failed ({}): {}", command.get_program().to_string_lossy(), output.status.signal().unwrap_or_else(|| output.status.code().unwrap_or(-1)), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8(output.stdout)?)
}
fn username() -> Result<String> { Ok(text_output(Command::new("id").arg("-un"))?.trim().to_owned()) }
fn unit() -> Result<String> { Ok(text_output(Command::new("systemd-escape").args(["--template=jasp-kiosk@.service", &username()?]))?.trim().to_owned()) }

fn read_regular(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(metadata.is_file(), "Refusing to use non-regular file {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("Inspecting {}", path.display())),
    }
    Ok(Some(fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?))
}

fn write_file(path: &Path, contents: &str, mode: u32) -> Result<()> {
    replace_file(path, contents, mode, |_| Ok(()))
}
fn checked_xinitrc(path: &Path, contents: &str, mode: u32) -> Result<()> {
    replace_file(path, contents, mode, |temp| {
        text_output(Command::new("/bin/bash").arg("-n").arg(temp)).map(|_| ())
    })
}
fn replace_file(path: &Path, contents: &str, mode: u32, check: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let dir = path.parent().context("Autostart path has no parent directory")?;
    fs::create_dir_all(dir)?;
    let temp = path.with_extension(format!("jasp-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&temp)
        .with_context(|| format!("Creating replacement for {}", path.display()))?;
    let result = (|| {
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        check(&temp)?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moode_hook_leaves_web_and_meter_commands_untouched_and_is_idempotent() {
        let vendor = format!("# /var/www/util/sysutil.sh\n{VENDOR_BRANCH}\tchromium --kiosk\nelif [ $PEPPY_SHOW = 1 ]; then\n\tpeppy\nfi\n");
        let updated = add_moode_hook(&vendor).unwrap().unwrap();
        assert_eq!(updated.replace(NATIVE_HOOK, ""), vendor);
        assert!(add_moode_hook(&updated).unwrap().is_none());
        assert!(add_moode_hook("#!/bin/sh\nother-launcher\n").is_err());
    }

    #[test]
    fn v2_migration_removes_the_old_default_and_keeps_chromium() {
        let previous = format!("{OLD_WEB}\t\texec chromium --kiosk\n{OLD_LAUNCH}elif [ $PEPPY_SHOW = 1 ]; then\n\tpeppy\nfi\n");
        let updated = add_moode_hook(&previous).unwrap().unwrap();
        assert_eq!(updated.replace(NATIVE_HOOK, ""), format!("{VENDOR_BRANCH}\t\texec chromium --kiosk\nelif [ $PEPPY_SHOW = 1 ]; then\n\tpeppy\nfi\n"));
    }
}
