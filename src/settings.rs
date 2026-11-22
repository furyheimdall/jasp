use crate::{model::{Command, Entry, Snapshot}, settings_web, theme::Theme};
use eframe::egui::{self, RichText, Ui};
use std::{borrow::Cow, fs, path::PathBuf};

#[path = "display.rs"]
mod display;
#[path = "settings_widgets.rs"]
pub(crate) mod widgets;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Language { #[default] English, Korean }

fn preference_path(name: &str) -> Option<PathBuf> {
    let absolute = |key| std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute());
    absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|home| home.join(".config")))
        .map(|path| path.join(env!("CARGO_PKG_NAME")).join(name))
}

fn read_preference(name: &str) -> Option<String> {
    fs::read_to_string(preference_path(name)?).ok()
}

fn save_preference(name: &str, value: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = preference_path(name).ok_or_else(|| std::io::Error::other("No user configuration directory"))?;
    fs::create_dir_all(path.parent().expect("configuration parent"))?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&temp)?;
    let result = (|| {
        writeln!(file, "{value}")?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result
}

pub(crate) fn tr<'a>(ui: &Ui, text: &'a str) -> Cow<'a, str> {
    if ui.ctx().data(|data| data.get_temp::<Language>(egui::Id::new("ui-language"))) != Some(Language::Korean) {
        return Cow::Borrowed(text);
    }
    Cow::Borrowed(match text {
        "SOURCE" => "재생 모드", "BROWSE" => "탐색", "Now playing" => "지금 재생", "Queue" => "대기열",
        "Library" => "라이브러리", "Settings" => "설정", "Outputs" => "오디오 출력", "Language" => "언어",
        "Sign out" => "로그아웃", "Sign in" => "로그인", "Server URL" => "서버 주소", "Username" => "사용자 이름",
        "Password" => "비밀번호", "Connect / Verify" => "서버 확인", "Refresh" => "새로 고침", "Rescan USB" => "USB 다시 검색",
        "Not connected" => "연결되지 않음",
        "Read only · Server playback and outputs cannot be changed" => "읽기 전용 · 서버 재생과 출력을 변경할 수 없어요.",
        "Invalid jastreamer server address." => "jastreamer 서버 주소가 올바르지 않아요.",
        "Could not reach the jastreamer server." => "jastreamer 서버에 연결할 수 없어요.",
        "This jastreamer server is not compatible." => "지원하지 않는 jastreamer 서버예요.",
        "jastreamer sign-in expired. Sign in again." => "jastreamer 세션이 만료됐어요. 다시 로그인하세요.",
        "Could not verify the saved jastreamer sign-in." => "저장된 jastreamer 로그인을 확인하지 못했어요.",
        "Could not read saved jastreamer settings." => "저장된 jastreamer 설정을 읽지 못했어요.",
        "Signed in, but could not save the jastreamer session." => "로그인했지만 jastreamer 세션을 저장하지 못했어요.",
        "The jastreamer USB output was released." => "jastreamer USB 출력 연결이 해제됐어요.",
        "USB output closed; server release was not confirmed." => "USB 출력을 닫았지만 서버에서의 해제는 확인하지 못했어요.",
        "jastreamer USB output error." => "jastreamer USB 출력에 문제가 생겼어요.",
        "jastreamer request failed." => "jastreamer 요청을 처리하지 못했어요.",
        "The selected jastreamer output is unavailable." => "선택한 jastreamer 출력을 사용할 수 없어요.",
        "jastreamer playback failed." => "jastreamer에서 재생하지 못했어요.",
        "The renderer reported a playback error. Press Play to resume." => "출력 장치에서 재생 오류가 발생했어요. 재생 버튼을 눌러 다시 시작하세요.",
        "Albums" => "앨범", "Artists" => "아티스트", "Genres" => "장르", "Folders" => "폴더", "Sources" => "음악 소스",
        "Playlists" => "재생목록", "Tracks" => "곡", "Most played" => "많이 재생한 곡", "Liked" => "좋아요", "Search" => "검색",
        "Keyboard" => "키보드", "Hide keys" | "Hide" => "숨기기", "Back" | "‹  Back" => "‹ 뒤로", "Up" => "상위 폴더",
        "Next  ›" => "다음 ›", "‹  Prev" => "‹ 이전", "Jump to current" => "현재 곡으로 이동", "Play all" => "전체 재생",
        "Play next" => "다음에 재생", "Add to queue" => "대기열에 추가", "Like track" => "좋아요",
        "Remove like" => "좋아요 취소", "Previous" => "이전 곡", "Next" => "다음 곡", "Play" => "재생",
        "Pause" => "일시정지", "Stop" => "정지", "PLAYING" => "재생 중", "NOW PLAYING" => "지금 재생 중",
        "PAUSED" => "일시정지", "STOPPED" => "정지", "STARTING" => "재생 준비 중", "ERROR" => "오류",
        "UNAVAILABLE" => "출력 연결 안 됨", "PLAYING ELSEWHERE" => "다른 출력에서 재생 중",
        "OUTPUT UNAVAILABLE" => "출력 연결 안 됨", "PLAYBACK FAILED" => "재생 실패", "No track selected" => "선택한 곡 없음",
        "FILE UNAVAILABLE" => "파일을 찾을 수 없음",
        "Search this collection" => "이 목록 검색", "Search music" => "음악 검색", "Search results" => "검색 결과",
        "Loading library…" => "라이브러리 불러오는 중…", "Refresh sources" => "소스 새로 고침",
        "Library settings" => "라이브러리 설정",
        "Sequential order" => "순차 재생", "Shuffle order" => "랜덤 재생",
        "Repeat off" => "반복 끄기", "Repeat one" => "한 곡 반복", "Repeat all" => "전체 반복",
        "Track details" => "곡 상세 정보", "Album artist" => "앨범 아티스트", "Duration" => "재생 시간",
        "Player" => "플레이어",
        "Release date" => "발매일", "Genre" => "장르", "Disc number" => "디스크 번호", "Track number" => "트랙 번호",
        "Format" => "포맷", "Sample rate" => "샘플레이트", "Bit depth" => "비트 깊이",
        "Channels" => "채널", "Bitrate" => "비트레이트", "Library path" => "라이브러리 경로",
        "Loading track details…" => "곡 상세 정보를 불러오는 중…",
        "Detailed tags could not be loaded." => "상세 태그를 불러오지 못했어요.",
        "moOde mode" => "moOde 모드",
        "Only hides moOde in Jasp. It does not stop the moOde server." =>
            "Jasp에서 moOde 모드만 숨겨요. moOde 서버는 중지하지 않아요.",
        "Next boot" => "다음 부팅 화면",
        "Could not save moOde mode" => "moOde 모드 설정을 저장하지 못했어요.",
        "moOde Web UI" => "moOde 웹 UI",
        "Read-only mode cannot change autostart." => "읽기 전용 모드에서는 자동 시작을 바꿀 수 없어요.",
        "Autostart" => "자동 시작",
        "Start Jasp automatically?" => "Jasp를 자동으로 시작할까요?",
        "Enable autostart" => "자동 시작 켜기",
        "Disable autostart" => "자동 시작 끄기",
        "Not now" => "지금은 안 함",
        "Updating autostart…" => "자동 시작 설정을 저장하는 중…",
        "Changes apply the next time you log in." => "다음 데스크톱 로그인부터 적용돼요.",
        "Jasp will replace the local moOde web screen on the next boot. The moOde server and playback settings will not change." =>
            "다음 부팅부터 로컬 화면에 moOde 웹 UI 대신 Jasp를 띄워요. moOde 서버나 재생 설정은 바뀌지 않아요.",
        "Jasp will open after you log in. Your desktop and other startup apps are unchanged." =>
            "데스크톱에 로그인하면 Jasp를 열어요. 데스크톱과 다른 자동 시작 앱은 바뀌지 않아요.",
        "Jasp will open full screen at boot. Confirm here, then approve sudo in the console. Jasp will reopen after approval." =>
            "부팅하면 Jasp가 전체 화면으로 열려요. 여기서 확인한 뒤 콘솔에서 sudo를 승인하면 Jasp가 다시 열려요.",
        "Changes apply on the next boot. The current player stays open." =>
            "다음 부팅부터 적용돼요. 지금 실행 중인 플레이어는 계속 열려 있어요.",
        "All sources" => "모든 소스", "Scan changes" => "변경 파일 검색", "Full rescan" => "전체 다시 검색",
        "Database scan is running…" => "음악 데이터베이스 검색 중…", "No indexed sources reported." => "인덱싱된 소스가 없어요.",
        "The selected source is scanned by MPD." => "선택한 소스를 MPD가 검색해요.",
        "Remember this server and stay signed in" => "서버 주소와 로그인 유지",
        "Allow unencrypted sign-in on this trusted LAN" => "이 신뢰하는 LAN에서 HTTP 로그인 허용",
        "USB devices" => "USB 오디오 장치", "Register for jastreamer" => "jastreamer에 등록", "Use this USB DAC" => "이 USB DAC 사용",
        "Release USB DAC" => "USB DAC 연결 해제", "Stop other output" => "다른 출력 정지", "Only the session is saved, never your password. Use HTTP only on a trusted LAN." => "비밀번호는 저장하지 않아요. HTTP는 신뢰할 수 있는 LAN에서만 사용하세요.",
        "Sign in to manage the jastreamer library." | "Sign in to view server library settings." => "라이브러리 설정을 보려면 jastreamer에 로그인하세요.",
        "Library sources" => "라이브러리 소스",
        "Manage sources in the jastreamer web UI." => "소스는 jastreamer 웹 UI에서 관리하세요.",
        "No library sources configured." => "등록된 라이브러리 소스가 없어요.",
        "Library scan" => "라이브러리 검색",
        "Incremental scan" => "변경 파일 검색", "Full scan" => "전체 검색",
        "Stop scan" => "검색 중단", "Processed" => "처리", "Updated" => "변경",
        "New files" => "신규 추가", "Deleted / missing" => "삭제·누락", "Errors" => "오류",
        "Scanning jastreamer library…" => "jastreamer 라이브러리 검색 중…",
        "jastreamer scan complete" => "jastreamer 검색 완료", "jastreamer scan cancelled" => "jastreamer 검색 취소",
        "jastreamer scan problem" => "jastreamer 검색 오류",
        "Library request could not be queued. Try again." => "라이브러리 요청을 보내지 못했어요. 다시 시도하세요.",
        "Read-only mode cannot start or stop scans." => "읽기 전용 모드에서는 검색을 시작하거나 중단할 수 없어요.",
        "Music sources" => "음악 소스", "Stored playlists" => "저장된 재생목록", "Play queue" => "대기열",
        "ALBUM" => "앨범", "albums" => "앨범", "track" | "tracks" => "곡",
        "Unknown artist" => "알 수 없는 아티스트", "Untitled track" => "제목 없는 곡",
        "Loading albums…" => "앨범 불러오는 중…", "Loading play queue…" => "대기열 불러오는 중…",
        "No tagged albums. Browse Folders for other music sources." => "앨범 정보가 있는 곡이 없어요. 폴더에서 음악을 찾아보세요.",
        "No matching tracks." => "일치하는 곡이 없어요.", "No stored playlists." => "저장된 재생목록이 없어요.",
        "No items in this location." => "이 위치에 항목이 없어요.", "The play queue is empty." => "대기열이 비어 있어요.",
        "Folder" => "폴더", "Playlist" => "재생목록", "Retry" => "다시 시도",
        "Queue is empty. Add tracks from Library." => "대기열이 비어 있어요. 라이브러리에서 곡을 추가하세요.",
        "Control the server queue or play music on a local USB DAC." => "서버 대기열을 제어하거나 로컬 USB DAC으로 음악을 재생하세요.",
        "Local player" => "로컬 플레이어", "Offline" => "연결 끊김", "Connecting" => "연결 중",
        "Official moOde settings" => "moOde 세부 설정",
        "These pages open in a private Chromium window. Playback and library pages are not available there." => "격리된 Chromium 창에 설정만 열려요. 재생 및 라이브러리 화면은 열 수 없어요.",
        "Network" => "네트워크", "System" => "시스템", "Audio" => "오디오",
        "Renderers" => "렌더러", "Preferences" => "환경설정", "Backup and restore" => "백업 및 복원",
        "System diagnostics" => "시스템 진단", "Renderer diagnostics" => "렌더러 진단",
        "Close settings window" => "세부 설정 창 닫기",
        "Server queue · playback is on another output" => "서버 대기열 · 다른 출력에서 재생 중",
        "Local USB audio · Direct output without a software mixer." => "로컬 USB 오디오 · 소프트웨어 믹서 없이 직접 출력",
        "No USB playback device detected." => "USB 오디오 장치가 없어요.",
        "EXTERNAL SOURCE" => "외부 소스", "Waiting for player state…" => "재생 상태를 불러오는 중…",
        "Pi USB DAC · Direct" => "Pi USB DAC · 직접 출력",
        "Back to app" => "앱으로 돌아가기", "Opening moOde settings…" => "moOde 설정을 여는 중…",
        "This Pi · One player can use the USB DAC at a time." => "이 Pi의 USB DAC는 한 번에 한 플레이어만 사용할 수 있어요.",
        "Press Play in moOde to switch the USB DAC from jastreamer." => "moOde에서 재생을 누르면 jastreamer의 USB DAC를 넘겨받을 수 있어요.",
        "Switch USB output to jastreamer?" => "USB 출력을 jastreamer로 전환할까요?",
        "Switch USB output to moOde?" => "USB 출력을 moOde로 전환할까요?",
        "If moOde is using this DAC, its playback will stop. The requested track will then play in jastreamer." =>
            "moOde가 이 DAC를 쓰고 있으면 재생을 멈춘 뒤, jastreamer에서 요청한 곡을 재생해요.",
        "The local jastreamer output will close before moOde starts the requested track. The server queue is kept." =>
            "jastreamer의 로컬 출력을 해제한 뒤 moOde에서 요청한 곡을 재생해요. 서버 대기열은 유지돼요.",
        "Choose a USB DAC" => "USB DAC 선택", "Switch and play" => "전환하고 재생", "Cancel" => "취소", "Close" => "닫기",
        "Play now" => "지금 재생", "Actions" => "곡·폴더 작업", "Move earlier" => "위로 이동",
        "Move later" => "아래로 이동", "Remove from queue" => "대기열에서 제거", "Queue position" => "대기열 순서",
        "Clear queue" => "전체 삭제", "Queue cleared" => "대기열을 비웠어요",
        "Clear the moOde queue?" => "moOde 대기열을 비울까요?",
        "Clear the jastreamer queue?" => "jastreamer 대기열을 비울까요?",
        "All moOde queue entries will be removed and playback will stop. Music files are kept."
            => "대기열의 모든 곡을 제거하고 재생을 멈춰요. 음악 파일은 삭제되지 않아요.",
        "All jastreamer queue entries will be removed. The current track can finish. Music files are kept."
            => "대기열의 모든 곡을 제거해요. 재생 중인 곡은 끝까지 들을 수 있고, 음악 파일은 삭제되지 않아요.",
        "The queue changed. Cancel and review the updated queue." => "대기열이 바뀌었어요. 취소하고 바뀐 목록을 확인해 주세요.",
        "moOde is unavailable; the DAC cannot be handed over safely." => "moOde에 연결할 수 없어 DAC를 안전하게 전환할 수 없어요.",
        "Checking the jastreamer track and USB output…" => "jastreamer의 곡과 USB 출력을 확인하는 중…",
        "Waiting for moOde to release the USB DAC…" => "moOde가 USB DAC를 해제하기를 기다리는 중…",
        "Starting jastreamer on the USB DAC…" => "USB DAC로 jastreamer 재생을 시작하는 중…",
        "Closing the jastreamer USB output…" => "jastreamer USB 출력을 해제하는 중…",
        "Could not switch USB output" => "USB 출력을 전환하지 못했어요.",
        "Refreshing music sources…" => "음악 소스를 불러오는 중…",
        "Music sources refreshed" => "음악 소스 새로 고침 완료", "Could not refresh music sources" => "음악 소스를 불러오지 못했어요.",
        "Scanning the music library…" => "라이브러리를 검색하는 중…",
        "Music scan complete" => "음악 검색 완료", "Music scan failed" => "음악 검색 실패",
        "source found" | "sources found" => "개 소스 확인", "track in the index" | "tracks in the index" => "곡 (라이브러리 전체)",
        "Audio signal path" => "오디오 신호 경로", "Decoding" => "디코딩", "MPD stream" => "MPD 스트림", "Output" => "출력",
        "Multiple USB DACs" => "여러 USB DAC", "More than one active ALSA stream" => "활성 ALSA 스트림 여러 개",
        "Format not reported" => "포맷 정보 없음", "Hardware format not observed" => "하드웨어 포맷 미확인",
        "Hardware format observed · Source format unknown" => "하드웨어 포맷 확인 · 원본 포맷 미확인",
        "Sample rate and channel count changed" => "샘플레이트와 채널 수가 변경됐어요.",
        "Sample rate changed" => "샘플레이트가 변경됐어요.", "Channel count changed" => "채널 수가 변경됐어요.",
        "Rate and channels match · Bit-perfect not verified" => "샘플레이트·채널 일치 · 비트 퍼펙트 미확인",
        "Bit-perfect not verified" => "비트 퍼펙트 미확인",
        "Bit-perfect to ALSA · No resampling, channel remix or DSP" => "ALSA까지 비트 퍼펙트 · 리샘플링·채널 변환·DSP 없음",
        "Bit-perfect PCM to ALSA · After decoding" => "디코딩 후 ALSA까지 비트 퍼펙트 PCM",
        "Bit-perfect to USB · Based on configuration" => "USB까지 비트 퍼펙트 · 설정 기준",
        "Lossless at this stage" => "이 단계의 무손실 확인", "Lossy source" => "손실 압축 원본",
        "PCM format changed" => "PCM 형식 변환 확인", "PCM bit depth decreased" => "PCM 비트 깊이가 감소했어요.",
        "Acknowledge" => "확인",
        "General" => "일반", "moOde settings" => "moOde 설정",
        "No MPD output is enabled." => "활성화된 MPD 출력이 없어요.",
        "MPD output not reported." => "MPD 출력 정보를 확인하지 못했어요.",
        "Configured MPD output" => "설정된 MPD 출력",
        "moOde uses its own audio configuration; this screen does not rewrite it." => "moOde의 기존 오디오 설정은 그대로 유지돼요.",
        "Select jastreamer in the sidebar and sign in to use the USB device there." => "jastreamer에서 이 USB 장치를 사용하려면 로그인하세요.",
        "Stop moOde playback before assigning this USB output to jastreamer." => "이 USB 출력을 jastreamer에 할당하려면 먼저 moOde 재생을 멈추세요.",
        "The registered USB device is not currently detected." => "등록된 USB 장치를 찾지 못했어요.",
        "Manage network outputs in the jastreamer web UI." => "네트워크 출력은 jastreamer 웹 UI에서 관리하세요.",
        "Active in jastreamer" => "jastreamer에서 사용 중", "Registered" => "등록됨",
        "Current server output" => "현재 서버 출력",
        "Launch moOde settings" => "moOde 설정 열기",
        "Touch keyboard" => "터치 키보드", "Library search" => "라이브러리 검색",
        "Shift" => "대문자", "Space" => "띄어쓰기", "Backspace" => "지우기",
        "USB audio" => "USB 오디오", "Switching output" => "오디오 출력 전환 중",
        "Green" => "녹색", "Orange" => "주황색", "Blue" => "파란색",
        "Monochrome" => "흑백", "Violet" => "보라색",
        "Display" => "화면",
        "Brightness" => "밝기", "When idle" => "입력이 없을 때", "System default" => "시스템 기본값",
        "Always on" => "항상 켜기", "Turn off after inactivity" => "시간 후 끄기",
        "Dim after inactivity" => "시간 후 어둡게", "Idle minutes" => "대기 시간",
        "Dimmed brightness" => "어두울 때 밝기", "Touch the screen to wake it." => "화면을 터치하면 원래 밝기로 돌아옵니다.",
        " min" => "분",
        "This display is managed by the desktop session." => "이 화면은 데스크톱의 화면 설정에서 관리합니다.",
        "Could not change the X11 idle timers." => "X11 화면 대기 시간을 변경하지 못했습니다.",
        "Could not set the display brightness." => "화면 밝기를 변경하지 못했습니다.",
        "Could not save display settings." => "화면 설정을 저장하지 못했습니다.",
        _ => text,
    })
}

pub(crate) struct View {
    pub(crate) display: display::Display,
    language: Language,
    moode_theme: Theme,
    jastreamer_theme: Theme,
    moode_enabled: bool,
    preference_error: Option<String>,
    sources: Vec<Entry>,
    source: String,
    web: settings_web::Viewer,
}

impl View {
    pub(crate) fn new(show_moode: bool, bare_x11: bool, lite: bool) -> Self {
        let language = if read_preference("language").is_some_and(|value| value.trim() == "ko") {
            Language::Korean
        } else { Language::English };
        let load_theme = |file, default| read_preference(file).and_then(|value| Theme::from_key(value.trim())).unwrap_or(default);
        Self {
            display: display::Display::new(bare_x11, lite),
            language, moode_theme: load_theme("theme-moode", Theme::Blue),
            jastreamer_theme: load_theme("theme-jastreamer", Theme::Green),
            moode_enabled: match read_preference("moode-mode").as_deref().map(str::trim) {
                Some("on") => true,
                Some("off") => false,
                _ => show_moode,
            },
            preference_error: None, sources: Vec::new(), source: String::new(), web: Default::default(),
        }
    }
    pub(crate) fn install_language(&self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("ui-language"), self.language));
    }

    pub(crate) fn theme(&self, jastreamer: bool) -> Theme {
        if jastreamer { self.jastreamer_theme } else { self.moode_theme }
    }

    pub(crate) fn moode_enabled(&self) -> bool { self.moode_enabled }

    pub(crate) fn app_options(&mut self, ui: &mut Ui) {
        widgets::card(ui, |ui| {
            if widgets::toggle(ui, "moOde mode", self.moode_enabled).clicked() {
                let enabled = !self.moode_enabled;
                match save_preference("moode-mode", if enabled { "on" } else { "off" }) {
                    Ok(()) => { self.moode_enabled = enabled; self.preference_error = None; ui.ctx().request_repaint(); }
                    Err(_) => self.preference_error = Some(tr(ui, "Could not save moOde mode").into_owned()),
                }
            }
            widgets::hint(ui, "Only hides moOde in Jasp. It does not stop the moOde server.");
        });
        self.show_preference_error(ui);
    }

    pub(crate) fn web_open(&self) -> bool { self.web.is_open() }
    pub(crate) fn poll_web(&mut self) { self.web.poll(); }
    pub(crate) fn close_web(&mut self) { self.web.close(); }

    pub(crate) fn general(&mut self, ui: &mut Ui) {
        let mut choice = self.language;
        widgets::section(ui, "Language", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                for (language, label) in [(Language::English, "English"), (Language::Korean, "한국어")] {
                    if widgets::choice(ui, label, choice == language).clicked() { choice = language; }
                }
            });
        });
        if choice != self.language {
            self.language = choice;
            let value = if choice == Language::Korean { "ko" } else { "en" };
            self.preference_error = save_preference("language", value).err().map(|error| format!("Could not save language: {error}"));
            self.install_language(ui.ctx());
            ui.ctx().request_repaint();
        }
        widgets::card(ui, |ui| {
            for (label, file, selected) in [
                ("moOde", "theme-moode", &mut self.moode_theme),
                ("jastreamer", "theme-jastreamer", &mut self.jastreamer_theme),
            ] {
                if file == "theme-moode" && !self.moode_enabled { continue; }
                ui.push_id(file, |ui| ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 4.0);
                    ui.add_sized([102.0, 42.0], egui::Label::new(RichText::new(label).size(16.0)));
                    for theme in Theme::ALL {
                        let active = *selected == theme;
                        if widgets::swatch(ui, theme.label(), active, theme.palette().accent).clicked() && !active {
                            *selected = theme;
                            self.preference_error = save_preference(file, theme.key()).err().map(|error| format!("Could not save theme: {error}"));
                            ui.ctx().request_repaint();
                        }
                    }
                }));
            }
        });
        self.show_preference_error(ui);
    }

    fn show_preference_error(&self, ui: &mut Ui) {
        if let Some(error) = &self.preference_error { ui.label(RichText::new(error).color(crate::ui::WARNING)); }
    }

    pub(crate) fn set_moode_sources(&mut self, sources: Vec<Entry>) {
        if !sources.iter().any(|entry| entry.path == self.source) { self.source.clear(); }
        self.sources = sources;
    }

    pub(crate) fn library(&mut self, ui: &mut Ui, snapshot: Option<&Snapshot>, can_edit: bool,
        (refresh_blocked, scanning, scan_failed): (bool, bool, bool)) -> Option<Command> {
        let mut action = None;
        widgets::section(ui, "Library sources", |ui| {
            ui.horizontal_wrapped(|ui| {
                let selected = self.sources.iter().find(|item| item.path == self.source)
                    .map_or_else(|| tr(ui, "All sources"), |entry| Cow::Borrowed(entry.label.as_str()));
                egui::ComboBox::from_id_salt("moode-scan-source").selected_text(RichText::new(selected).size(17.0))
                    .width(200.0).show_ui(ui, |ui| {
                        if widgets::choice(ui, "All sources", self.source.is_empty()).clicked() {
                            self.source.clear();
                            ui.close();
                        }
                        for item in &self.sources {
                            let clicked = ui.push_id(&item.path, |ui| widgets::choice(ui, &item.label, self.source == item.path).clicked()).inner;
                            if clicked {
                                self.source.clone_from(&item.path);
                                ui.close();
                            }
                        }
                    });
                if ui.add_enabled_ui(!refresh_blocked, |ui| widgets::button(ui, "Refresh sources", false)).inner.clicked() {
                    action = Some(Command::LoadLibrarySettings { notify: true });
                }
            });
            if self.sources.is_empty() { widgets::hint(ui, "No indexed sources reported."); }
        });
        let updating = scanning || snapshot.is_some_and(|snapshot| snapshot.library_updating);
        widgets::section(ui, "Library scan", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(can_edit && !updating && !scan_failed, |ui| {
                    if widgets::button(ui, "Scan changes", true).clicked() {
                        action = Some(Command::ScanLibrary { path: self.source.clone(), full: false });
                    }
                    if widgets::button(ui, "Full rescan", false).clicked() {
                        action = Some(Command::ScanLibrary { path: self.source.clone(), full: true });
                    }
                });
            });
            widgets::hint(ui, if updating { "Database scan is running…" } else { "The selected source is scanned by MPD." });
        });
        action
    }

    pub(crate) fn official(&mut self, ui: &mut Ui, origin: &str) {
        settings_web::show(ui, origin, &mut self.web);
    }
}
