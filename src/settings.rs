use crate::{chrome, model::{Command, Entry, RepeatMode, Snapshot}, settings_web, theme::{ON_ACCENT, Theme}};
use eframe::egui::{self, RichText, Ui};
use std::{borrow::Cow, fs, path::PathBuf};

#[path = "display.rs"]
mod display;

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
        "Playback" => "재생", "Repeat" => "반복 재생", "Shuffle" => "무작위 재생", "Library settings" => "라이브러리 설정",
        "Sequential order" => "순차 재생", "Shuffle order" => "랜덤 재생",
        "Repeat off" => "반복 끄기", "Repeat one" => "한 곡 반복", "Repeat all" => "전체 반복",
        "Track details" => "곡 상세 정보", "Album artist" => "앨범 아티스트", "Duration" => "재생 시간",
        "Player" => "플레이어",
        "Release date" => "발매일", "Genre" => "장르", "Disc number" => "디스크 번호", "Track number" => "트랙 번호",
        "Format" => "포맷", "Sample rate" => "샘플레이트", "Bit depth" => "비트 깊이",
        "Channels" => "채널", "Bitrate" => "비트레이트", "Library path" => "라이브러리 경로",
        "Loading track details…" => "곡 상세 정보를 불러오는 중…",
        "Detailed tags could not be loaded." => "상세 태그를 불러오지 못했어요.",
        "Enable moOde mode" => "moOde 모드 사용",
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
        "Paths refer to directories on the jastreamer server." => "경로는 jastreamer 서버의 디렉터리를 가리켜요.",
        "No library sources configured." => "등록된 라이브러리 소스가 없어요.",
        "Server configuration changed. Discard edits to load the current sources." => "서버 설정이 변경됐어요. 편집을 취소하면 최신 소스를 불러와요.",
        "Remove source" => "소스 제거", "Name" => "이름", "Server path" => "서버 경로",
        "Add source" => "소스 추가", "Save sources" => "소스 저장", "Discard edits" => "편집 취소",
        "Wait for the active scan before saving sources." => "진행 중인 검색이 끝난 뒤 소스를 저장하세요.",
        "Every source needs a name and server path." => "모든 소스에 이름과 서버 경로를 입력하세요.",
        "Saving removes the selected sources from the library. It does not delete media files." => "선택한 소스가 라이브러리에서 제거돼요. 음악 파일은 삭제하지 않아요.",
        "Confirm removal and save" => "제거 확인 후 저장",
        "Library scan" => "라이브러리 검색",
        "Incremental scan" => "변경 파일 검색", "Full scan" => "전체 검색",
        "Save or discard source edits before starting a scan." => "검색을 시작하기 전에 소스 편집을 저장하거나 취소하세요.",
        "Stop scan" => "검색 중단", "Processed" => "처리", "Updated" => "변경",
        "New files" => "신규 추가", "Deleted / missing" => "삭제·누락", "Errors" => "오류",
        "Scanning jastreamer library…" => "jastreamer 라이브러리 검색 중…",
        "jastreamer scan complete" => "jastreamer 검색 완료", "jastreamer scan cancelled" => "jastreamer 검색 취소",
        "jastreamer scan problem" => "jastreamer 검색 오류",
        "Library sources saved. No scan was started." => "소스를 저장했어요. 검색은 아직 시작하지 않았어요.",
        "Could not create a library source ID." => "라이브러리 소스 ID를 만들 수 없어요.",
        "Library request could not be queued. Try again." => "라이브러리 요청을 보내지 못했어요. 다시 시도하세요.",
        "Read-only mode cannot change library settings or start or stop scans." => "읽기 전용 모드에서는 라이브러리 설정을 변경하거나 검색을 제어할 수 없어요.",
        "Server configuration changed. Refresh before saving library sources." => "서버 설정이 변경됐어요. 새로 고침 후 소스를 저장하세요.",
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
        "General" => "일반", "Back to settings" => "설정으로 돌아가기",
        "USB DAC and player output" => "USB DAC 및 플레이어 출력",
        "Music sources and library scans" => "음악 소스 및 라이브러리 검색",
        "System, network, audio and other moOde options" => "시스템·네트워크·오디오 등 moOde 설정",
        "Touch keyboard" => "터치 키보드", "Library search" => "라이브러리 검색",
        "Shift" => "대문자", "Space" => "띄어쓰기", "Backspace" => "지우기",
        "USB audio" => "USB 오디오", "Switching output" => "오디오 출력 전환 중",
        "moOde theme" => "moOde 테마", "jastreamer theme" => "jastreamer 테마",
        "Green" => "녹색", "Orange" => "주황색", "Blue" => "파란색",
        "Monochrome" => "흑백", "Violet" => "보라색",
        "Display" => "화면", "Brightness and idle behavior" => "밝기 및 화면 꺼짐",
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
        ui.label(RichText::new("jastreamer · moOde").size(12.0).color(crate::ui::MUTED));
        ui.add_space(4.0);
        let mut enabled = self.moode_enabled;
        if ui.checkbox(&mut enabled, tr(ui, "Enable moOde mode")).changed() {
            match save_preference("moode-mode", if enabled { "on" } else { "off" }) {
                Ok(()) => { self.moode_enabled = enabled; self.preference_error = None; ui.ctx().request_repaint(); }
                Err(_) => self.preference_error = Some(tr(ui, "Could not save moOde mode").into_owned()),
            }
        }
        ui.label(RichText::new(tr(ui, "Only hides moOde in Jasp. It does not stop the moOde server."))
            .size(12.0).color(crate::ui::MUTED));
    }

    pub(crate) fn web_open(&self) -> bool { self.web.is_open() }
    pub(crate) fn poll_web(&mut self) { self.web.poll(); }
    pub(crate) fn close_web(&mut self) { self.web.close(); }

    pub(crate) fn general(&mut self, ui: &mut Ui) {
        let mut choice = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.label(tr(ui, "Language"));
            for (language, label) in [(Language::English, "English"), (Language::Korean, "한국어")] {
                if crate::ui::button(ui, label, choice == language).clicked() { choice = language; }
            }
        });
        if choice != self.language {
            self.language = choice;
            let value = if choice == Language::Korean { "ko" } else { "en" };
            self.preference_error = save_preference("language", value).err().map(|error| format!("Could not save language: {error}"));
            self.install_language(ui.ctx());
            ui.ctx().request_repaint();
        }
        for (label, file, selected) in [
            ("moOde theme", "theme-moode", &mut self.moode_theme),
            ("jastreamer theme", "theme-jastreamer", &mut self.jastreamer_theme),
        ] {
            if file == "theme-moode" && !self.moode_enabled { continue; }
            ui.add_space(5.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_sized([112.0, 43.0], egui::Label::new(tr(ui, label)));
                for theme in Theme::ALL {
                    let palette = theme.palette();
                    let active = *selected == theme;
                    let label = RichText::new(tr(ui, theme.label())).color(if active { ON_ACCENT } else { palette.accent });
                    let control = egui::Button::new(label).min_size(egui::vec2(65.0, 43.0)).corner_radius(8)
                        .fill(if active { palette.accent } else { palette.card })
                        .stroke(egui::Stroke::new(1.0, palette.accent.gamma_multiply(0.50)));
                    if ui.add(control).clicked() && !active {
                        *selected = theme;
                        self.preference_error = save_preference(file, theme.key()).err().map(|error| format!("Could not save theme: {error}"));
                        ui.ctx().request_repaint();
                    }
                }
            });
        }
        if let Some(error) = &self.preference_error { ui.label(RichText::new(error).color(crate::ui::WARNING)); }
    }

    pub(crate) fn set_moode_sources(&mut self, sources: Vec<Entry>) {
        if !sources.iter().any(|entry| entry.path == self.source) { self.source.clear(); }
        self.sources = sources;
    }

    pub(crate) fn playback(ui: &mut Ui, snapshot: Option<&Snapshot>, can_edit: bool) -> Option<Command> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            if let Some(snapshot) = snapshot {
                let mut random = snapshot.random;
                ui.label(tr(ui, "Repeat"));
                ui.add_enabled_ui(can_edit, |ui| {
                    egui::ComboBox::from_id_salt("mpd-repeat")
                        .selected_text(tr(ui, chrome::repeat_label(snapshot.repeat)))
                        .show_ui(ui, |ui| {
                            for mode in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
                                if ui.selectable_label(snapshot.repeat == mode, tr(ui, chrome::repeat_label(mode))).clicked()
                                    && snapshot.repeat != mode {
                                    action = Some(Command::SetRepeat(mode));
                                }
                            }
                        });
                });
                if ui.add_enabled(can_edit, egui::Checkbox::new(&mut random, tr(ui, "Shuffle"))).changed() {
                    action = Some(Command::SetRandom(random));
                }
            }
        });
        action
    }

    pub(crate) fn library(&mut self, ui: &mut Ui, snapshot: Option<&Snapshot>, can_edit: bool,
        (refresh_blocked, scanning, scan_failed): (bool, bool, bool)) -> Option<Command> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            let selected = self.sources.iter().find(|item| item.path == self.source)
                .map_or_else(|| tr(ui, "All sources"), |entry| Cow::Borrowed(entry.label.as_str()));
            egui::ComboBox::from_id_salt("moode-scan-source").selected_text(selected).width(180.0).show_ui(ui, |ui| {
                if ui.selectable_label(self.source.is_empty(), tr(ui, "All sources")).clicked() { self.source.clear(); }
                for item in &self.sources {
                    if ui.selectable_label(self.source == item.path, &item.label).clicked() { self.source.clone_from(&item.path); }
                }
            });
            if ui.add_enabled_ui(!refresh_blocked, |ui| crate::ui::button(ui, "Refresh sources", false)).inner.clicked() {
                action = Some(Command::LoadLibrarySettings { notify: true });
            }
        });
        let updating = scanning || snapshot.is_some_and(|snapshot| snapshot.library_updating);
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(can_edit && !updating && !scan_failed, |ui| {
                if crate::ui::button(ui, "Scan changes", true).clicked() {
                    action = Some(Command::ScanLibrary { path: self.source.clone(), full: false });
                }
                if crate::ui::button(ui, "Full rescan", false).clicked() {
                    action = Some(Command::ScanLibrary { path: self.source.clone(), full: true });
                }
            });
        });
        ui.label(tr(ui, if updating { "Database scan is running…" } else if self.sources.is_empty() {
            "No indexed sources reported."
        } else { "The selected source is scanned by MPD." }));
        action
    }

    pub(crate) fn official(&mut self, ui: &mut Ui, origin: &str) {
        settings_web::show(ui, origin, &mut self.web);
    }
}
