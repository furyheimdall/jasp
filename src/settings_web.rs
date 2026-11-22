use crate::settings::{tr, widgets};
use eframe::egui;
use reqwest::Url;
use serde_json::json;
use std::{
    env,
    ffi::OsString,
    fs::{self, DirBuilder, File},
    io::{self, Read},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) const RAIL_PX: f32 = 180.0;

// Unlabelled pages are reachable only from the official settings pages. In
// particular, this list includes neither the player nor the library settings.
const PAGES: &[(&str, Option<&str>)] = &[
    ("/net-config.php", Some("Network")),
    ("/sys-config.php", Some("System")),
    ("/snd-config.php", Some("Audio")),
    ("/mpd-config.php", Some("MPD")),
    ("/cdsp-config.php", Some("DSP")),
    ("/ren-config.php", Some("Renderers")),
    ("/per-config.php", Some("Preferences")),
    ("/bkp-config.php", Some("Backup and restore")),
    ("/sys-status.php", Some("System diagnostics")),
    ("/ren-status.php", Some("Renderer diagnostics")),
    ("/gpio-config.php", None),
    ("/cdsp-configeditor.php", None),
    ("/chp-config.php", None),
    ("/eqg-config.php", None),
    ("/eqp-config.php", None),
    ("/blu-config.php", None),
    ("/apl-config.php", None),
    ("/spo-config.php", None),
    ("/sqe-config.php", None),
    ("/upp-config.php", None),
    ("/trx-config.php", None),
    ("/dez-config.php", None),
    ("/inp-config.php", None),
    ("/peppy-config.php", None),
    ("/sys-restored.php", None),
];

const SETTINGS_CSS: &str = r#"
#panel-header { display: none !important; }
.ui-page { padding-top: 0 !important; }
"#;

const BOOTSTRAP_HTML: &str = r#"<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Official moOde settings</title>
<body><h1>Official moOde settings</h1>
<p>The protected page opens only after the navigation guard starts.</p>
<p>If this page stays open, close the window and return to moOde Native.</p></body></html>"#;
// Chromium starts with a local file, never an unguarded web document.
const BOOTSTRAP_JS: &str = r#"
async function openSettings() {
    const dnr = chrome.declarativeNetRequest;
    await dnr.updateSessionRules({
        removeRuleIds: settings.rules.map(rule => rule.id),
        addRules: settings.rules,
    });
    const check = async (url, type, expected) => {
        const { matchedRules } = await dnr.testMatchOutcome({ url, type });
        if (matchedRules.length !== (expected === null ? 0 : 1) ||
            (expected !== null && matchedRules[0].ruleId !== expected)) {
            throw new Error('Settings navigation guard did not match');
        }
    };
    await Promise.all([
        check(settings.target, 'main_frame', settings.targetRule),
        check(settings.target + '?setting=value', 'main_frame', settings.targetRule),
        check(settings.target + '/', 'main_frame', 1),
        check(settings.origin + '/', 'main_frame', 1),
        check(settings.origin + '/index.php', 'main_frame', 1),
        check(settings.origin + '/index.php', 'sub_frame', 1),
        check('https://moode-settings.invalid/other', 'main_frame', 1),
        check(settings.origin + '/', 'xmlhttprequest', null),
    ]);
    // moOde's landing request establishes the PHP session used by its settings.
    // Fetch only for the cookie; the response never becomes a browser document.
    const session = await fetch(settings.origin + '/index.php', {
        credentials: 'include', redirect: 'error', cache: 'no-store',
        signal: AbortSignal.timeout(6000),
    });
    if (!session.ok) throw new Error('Could not initialize the moOde settings session');
    await session.body?.cancel();
    // The app tab may be created just after the extension is installed.
    for (let attempt = 0; attempt < 40; attempt++) {
        const tab = (await chrome.tabs.query({})).find(t =>
            t.url === settings.bootstrap || t.pendingUrl === settings.bootstrap);
        if (tab) {
            await chrome.tabs.update(tab.id, { url: settings.target });
            return;
        }
        await new Promise(resolve => setTimeout(resolve, 100));
    }
}
chrome.runtime.onInstalled.addListener(() => openSettings().catch(console.error));
"#;

#[derive(Default)]
pub(crate) struct Viewer {
    browser: Option<Browser>,
    error: Option<&'static str>,
}

impl Viewer {
    pub(crate) fn is_open(&self) -> bool { self.browser.is_some() }
    pub(crate) fn close(&mut self) { self.browser = None; }

    pub(crate) fn poll(&mut self) {
        let Some(browser) = &self.browser else { return; };
        self.error = match browser.result.try_recv() {
            Ok(result) => result.err(),
            Err(TryRecvError::Disconnected) => Some("The settings window stopped unexpectedly."),
            Err(TryRecvError::Empty) => return,
        };
        self.browser = None;
    }
}

pub(crate) fn show(ui: &mut egui::Ui, moode_origin: &str, web: &mut Viewer) {
    widgets::section(ui, "Launch moOde settings", |ui| {
        widgets::hint(ui, "These pages open in a private Chromium window. Playback and library pages are not available there.");
        let selected = ui.add_enabled_ui(web.browser.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                PAGES.iter().enumerate().filter_map(|(index, (_, label))| {
                    let label = label.as_ref()?;
                    widgets::button(ui, label, false).clicked().then_some(index)
                }).last()
            }).inner
        }).inner;
        if let Some(index) = selected {
            match Browser::start(moode_origin, index, ui.ctx().clone()) {
                Ok(browser) => {
                    web.browser = Some(browser);
                    web.error = None;
                }
                Err(error) => web.error = Some(error),
            }
        }
        if let Some(error) = web.error {
            ui.colored_label(ui.visuals().error_fg_color, tr(ui, error));
        }
    });
}

type WindowResult = Result<(), &'static str>;

struct Browser {
    stop: Sender<()>,
    result: Receiver<WindowResult>,
    worker: Option<JoinHandle<()>>,
}

impl Browser {
    fn start(origin: &str, index: usize, ctx: egui::Context) -> Result<Self, &'static str> {
        let url = Url::parse(origin).map_err(|_| "The configured moOde URL is invalid.")?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err("The configured moOde URL must use HTTP or HTTPS.");
        }
        // Do not send credentials, path, query or fragment from the backend URL
        // to Chromium, and never accept a CLI-supplied settings path.
        let origin = url.origin().ascii_serialization();
        let (stop, stopping) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("moode-settings".into())
            .spawn(move || {
                let outcome = Self::run(&origin, index, &stopping);
                let _ = done.send(outcome);
                ctx.request_repaint();
            })
            .map_err(|_| "Could not start the settings launcher.")?;
        Ok(Self { stop, result, worker: Some(worker) })
    }

    fn run(origin: &str, index: usize, stop: &Receiver<()>) -> WindowResult {
        let profile = PrivateDir::new().map_err(|_| "Could not create a private Chromium profile.")?;
        let extension = profile.0.join("guard");
        let bootstrap = Url::from_file_path(profile.0.join("start.html"))
            .map_err(|_| "Could not prepare the settings launch page.")?;
        fs::write(profile.0.join("start.html"), BOOTSTRAP_HTML)
            .map_err(|_| "Could not prepare the settings launch page.")?;
        write_guard(&extension, origin, index, &bootstrap)
            .map_err(|_| "Could not prepare the settings navigation guard.")?;
        if !matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
            return Ok(());
        }
        let mut child = launch(&profile.0.join("browser"), &extension, &bootstrap).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "Chromium is required to open official settings."
            } else {
                "Could not launch the private Chromium window."
            }
        })?;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => return Err("The private Chromium window exited with an error."),
                Ok(None) => {}
                Err(_) => {
                    stop_child(&mut child);
                    return Err("Could not monitor the private Chromium window.");
                }
            }
            if !matches!(stop.recv_timeout(Duration::from_millis(250)), Err(mpsc::RecvTimeoutError::Timeout)) {
                stop_child(&mut child);
                return Ok(());
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> io::Result<Self> {
        let mut nonce = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        // Prefer volatile per-user storage. Chromium parses comma-separated
        // extension paths, so don't pass a directory containing a comma.
        let valid = |path: &Path| {
            path.is_absolute() && !path.as_os_str().as_encoded_bytes().contains(&b',')
        };
        let parent = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| valid(path) && path.is_dir())
            .unwrap_or_else(|| {
                let path = env::temp_dir();
                if valid(&path) { path } else { PathBuf::from("/tmp") }
            });
        let name = format!("moode-settings-{:032x}", u128::from_ne_bytes(nonce));
        let path = parent.join(name);
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn write_guard(extension: &Path, origin: &str, index: usize, bootstrap: &Url) -> io::Result<()> {
    fs::create_dir(extension)?;
    let endpoint = Url::parse(origin).map_err(io::Error::other)?;
    let host = endpoint.host_str().ok_or_else(|| io::Error::other("Missing moOde host"))?;
    let host_permission = format!("{}://{host}/*", endpoint.scheme());
    let port = endpoint.port_or_known_default().ok_or_else(|| io::Error::other("Missing moOde port"))?;
    // Chromium matches the request path including its query. Match a bare
    // allowlisted path or that same path with a query, not arbitrary suffixes.
    let settings_matches: Vec<_> = PAGES.iter().flat_map(|(path, _)| {
        let page = format!("{}://{host}:{port}{path}", endpoint.scheme());
        let with_query = format!("{page}?*");
        [page, with_query]
    }).collect();
    let manifest = json!({
        "manifest_version": 3,
        "name": "moOde Native Settings Guard",
        "version": "1.0",
        "minimum_chrome_version": "103",
        "permissions": ["tabs", "declarativeNetRequest", "declarativeNetRequestFeedback"],
        "host_permissions": [host_permission],
        "background": {"service_worker": "start.js"},
        "content_scripts": [{
            "matches": settings_matches,
            "css": ["settings.css"],
            "run_at": "document_start"
        }],
        "declarative_net_request": {
            "rule_resources": [{"id": "deny", "enabled": true, "path": "deny.json"}]
        }
    });
    let documents = ["main_frame", "sub_frame"];
    // Deny web documents by default, including HTTP redirects and new tabs.
    // Loading images/scripts and making settings fetch/XHR requests is unaffected.
    let deny = json!([{
        "id": 1,
        "priority": 1,
        "action": {"type": "block"},
        "condition": {"regexFilter": "^https?://", "resourceTypes": documents}
    }]);
    let rules: Vec<_> = PAGES.iter().enumerate().map(|(i, (path, _))| {
        let mut filter = String::from("^");
        for ch in origin.chars().chain(path.chars()) {
            if ".+*?()|[]{}^$\\".contains(ch) {
                filter.push('\\');
            }
            filter.push(ch);
        }
        filter.push_str(r"(?:\?[^#]*)?(?:#.*)?$");
        json!({
            "id": i + 2,
            "priority": 2,
            "action": {"type": "allow"},
            "condition": {
                "regexFilter": filter,
                "isUrlFilterCaseSensitive": true,
                "resourceTypes": documents
            }
        })
    }).collect();
    let settings = json!({
        "origin": origin,
        "bootstrap": bootstrap.as_str(),
        "target": format!("{origin}{}", PAGES[index].0),
        "targetRule": index + 2,
        "rules": rules
    });
    fs::write(extension.join("manifest.json"), manifest.to_string())?;
    fs::write(extension.join("deny.json"), deny.to_string())?;
    fs::write(extension.join("settings.css"), SETTINGS_CSS)?;
    fs::write(extension.join("start.js"), format!("const settings = {settings};\n{BOOTSTRAP_JS}"))
}

fn launch(profile: &Path, extension: &Path, bootstrap: &Url) -> io::Result<Child> {
    fn path_flag(flag: &str, path: &Path) -> OsString {
        let mut arg = OsString::from(flag);
        arg.push(path);
        arg
    }
    let args = [
        path_flag("--user-data-dir=", profile),
        path_flag("--disable-extensions-except=", extension),
        path_flag("--load-extension=", extension),
        OsString::from(format!("--app={bootstrap}")),
        OsString::from(format!("--window-position={RAIL_PX},0")),
    ];
    let mut last_error = None;
    for binary in ["chromium", "chromium-browser"] {
        match Command::new(binary)
            .args(&args)
            .args([
                "--window-size=1080,700",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-mode",
                "--disable-background-networking",
                "--disable-default-apps",
                "--disable-sync",
                "--password-store=basic",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::NotFound => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}
