use anyhow::{Context, Result, bail, ensure};
use std::{
    fs, io,
    net::{IpAddr, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(crate) struct PlaybackPcm(PathBuf);

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ObservedPcm {
    /// The stable ALSA card ID, not an inferred product label.
    pub(crate) device: String,
    pub(crate) rate: u32,
    pub(crate) channels: u32,
    pub(crate) format: String,
    #[serde(skip)]
    pub(crate) direct: bool,
}

/// Configured USB playback substreams owned by the MPD on this local connection.
/// The caller has confirmed that an ALSA output is enabled.
pub(crate) fn observed_mpd_usb_pcm(stream: &TcpStream, direct_output: Option<&str>) -> Vec<ObservedPcm> {
    if !stream.peer_addr().is_ok_and(|addr| addr.ip().to_canonical().is_loopback())
        || !stream.local_addr().is_ok_and(|addr| addr.ip().to_canonical().is_loopback()) {
        return Vec::new();
    }
    let Ok(pcms) = fs::read_to_string("/proc/asound/pcm") else { return Vec::new() };
    let mut usb_devices = None;
    let mut observed = Vec::new();
    for line in pcms.lines() {
        let Some((index, _)) = line.split_once(':') else { continue };
        let Some((card, dev)) = index.split_once('-') else { continue };
        let (Ok(card), Ok(dev)) = (card.parse::<u32>(), dev.parse::<u32>()) else { continue };
        let card_path = Path::new("/proc/asound").join(format!("card{card}"));
        let Ok(entries) = fs::read_dir(card_path.join(format!("pcm{dev}p"))) else { continue };
        let mut card_id = None;
        for entry in entries.flatten() {
            if !is_substream(entry.file_name().to_str()) { continue; }
            let mut path = entry.path();
            path.push("status");
            let Some(pid) = read_status_pid(&path).ok().flatten() else { continue };
            path.set_file_name("hw_params");
            let Ok(params) = fs::read_to_string(&path) else { continue };
            let Some((rate, channels, format)) = pcm_params(&params) else { continue };
            let Ok(owner) = mpd_process(pid) else { continue };
            let Some(id) = card_id.get_or_insert_with(|| {
                let id = fs::read_to_string(card_path.join("id")).ok()?;
                let card = id.trim();
                if !valid_card_id(card) { return None; }
                usb_devices.get_or_insert_with(super::devices).iter().any(|device| {
                    hardware_selector(device).is_some_and(|(id, number)| id == card && number.parse::<u32>() == Ok(dev))
                }).then_some(id)
            }) else { continue };
            if verify_mpd_peer(stream, owner, true).is_err() { continue; }
            // Reading hw_params and verifying the socket are not atomic with
            // ALSA ownership. Discard samples taken across an observed handoff.
            path.set_file_name("status");
            if read_status_pid(&path).ok().flatten() != Some(pid)
                || mpd_process(pid).ok() != Some(owner) {
                continue;
            }
            let id = id.trim();
            let direct = direct_output.is_some_and(|name| super::mpd_route::direct(owner, name, card, dev, id));
            observed.push(ObservedPcm { device: id.to_owned(), rate, channels, format: format.to_owned(), direct });
        }
    }
    observed
}

fn pcm_params(params: &str) -> Option<(u32, u32, &str)> {
    // The kernel prints the integer rate followed by the exact ratio in parentheses.
    let rate = field(params, "rate")?.split_ascii_whitespace().next().and_then(positive_u32)?;
    let channels = positive_u32(field(params, "channels")?)?;
    let format = field(params, "format")?;
    if format.is_empty() || !format.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }
    Some((rate, channels, format))
}

fn valid_card_id(card: &str) -> bool {
    !card.is_empty() && card.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn hardware_selector(device: &str) -> Option<(&str, &str)> {
    device.strip_prefix("hw:CARD=")?.split_once(",DEV=")
}

fn is_substream(name: Option<&str>) -> bool {
    name.and_then(|name| name.strip_prefix("sub"))
        .is_some_and(|sub| !sub.is_empty() && sub.bytes().all(|b| b.is_ascii_digit()))
}

fn read_status_pid(path: &Path) -> Result<Option<u32>> {
    (|| status_pid(&fs::read_to_string(path)?))()
        .with_context(|| format!("Reading PCM ownership at {}", path.display()))
}

fn mpd_process(pid: u32) -> Result<u32> {
    // ALSA can report the opening thread, whose comm need not be "mpd".
    let process = fs::read_to_string(format!("/proc/{pid}/status"))
        .with_context(|| format!("Cannot identify USB PCM owner PID {pid}"))?;
    let tgid = field(&process, "Tgid").and_then(positive_u32)
        .with_context(|| format!("Unknown process for USB PCM owner PID {pid}"))?;
    let name = fs::read_to_string(format!("/proc/{tgid}/comm"))
        .with_context(|| format!("Cannot identify USB PCM owner process {tgid}"))?;
    let name = name.trim_end_matches('\n');
    ensure!(name == "mpd", "USB DAC is held by {name:?} (PID {tgid}), not MPD");
    Ok(tgid)
}

fn positive_u32(text: &str) -> Option<u32> { text.parse().ok().filter(|&value| value > 0) }

fn status_pid(status: &str) -> Result<Option<u32>> {
    if status.trim() == "closed" { return Ok(None); }
    ensure!(matches!(field(status, "state"), Some("OPEN" | "SETUP" | "PREPARED" | "RUNNING" |
        "XRUN" | "DRAINING" | "PAUSED" | "SUSPENDED")), "Unknown or disconnected kernel PCM state");
    Ok(Some(field(status, "owner_pid").and_then(positive_u32).context("Unknown kernel PCM owner PID")?))
}

fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim_end() == name).then_some(value.trim())
    })
}

impl PlaybackPcm {
    pub(crate) fn for_device(device: &str) -> Result<Self> {
        let (card, dev) = hardware_selector(device)
            .context("Expected an exact USB ALSA playback device (hw:CARD=…,DEV=…)")?;
        ensure!(valid_card_id(card)
            && !dev.is_empty() && dev.bytes().all(|b| b.is_ascii_digit()), "Invalid USB ALSA playback device");
        ensure!(super::devices().iter().any(|candidate| candidate == device),
            "USB ALSA playback device is not available: {device}");
        Ok(Self(Path::new("/proc/asound").join(card).join(format!("pcm{dev}p"))))
    }

    pub(crate) fn mpd_owner(&self) -> Result<Option<u32>> {
        let entries = fs::read_dir(&self.0).with_context(|| format!("Reading PCM substreams at {}", self.0.display()))?;
        let mut known = false;
        let mut owner = None;
        for entry in entries {
            let entry = entry?;
            if !is_substream(entry.file_name().to_str()) { continue; }
            known = true;
            let path = entry.path().join("status");
            if let Some(pid) = read_status_pid(&path)? {
                let tgid = mpd_process(pid)?;
                ensure!(owner.is_none_or(|current| current == tgid), "USB DAC is held by multiple MPD instances");
                owner = Some(tgid);
            }
        }
        ensure!(known, "No kernel PCM substream status is available for {}", self.0.display());
        Ok(owner)
    }

    pub(crate) fn wait_released(&self, owner: u32) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.mpd_owner()? {
                None => return Ok(()),
                Some(current) if current == owner => (),
                Some(_) => bail!("USB DAC was acquired by a different MPD instance"),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "MPD still holds the USB DAC after 1 second");
            thread::sleep(remaining.min(Duration::from_millis(25)));
        }
    }
}

pub(crate) fn verify_mpd_peer(stream: &TcpStream, owner: u32, enabled_alsa: bool) -> Result<()> {
    let client = stream.local_addr()?;
    let server = stream.peer_addr()?;
    let client_ip = client.ip().to_canonical();
    let server_ip = server.ip().to_canonical();
    ensure!(client_ip.is_loopback() && server_ip.is_loopback(), "MPD connection is not local loopback");
    // Find the accepted socket, not our end. IPv4 peers can be reported in tcp6
    // when the listener is dual-stack.
    let mut accepted = None;
    if let (IpAddr::V4(server_ip), IpAddr::V4(client_ip)) = (server_ip, client_ip) {
        let endpoint = |ip: std::net::Ipv4Addr, port| format!("{:08X}:{port:04X}", u32::from_ne_bytes(ip.octets()));
        accepted = socket_entry("/proc/net/tcp", &endpoint(server_ip, server.port()),
            &endpoint(client_ip, client.port()))?;
    }
    let (uid, inode) = match accepted {
        Some(socket) => socket,
        None => socket_entry("/proc/net/tcp6", &ipv6_endpoint(server_ip, server.port()),
            &ipv6_endpoint(client_ip, client.port()))?.context("Cannot identify the selected local MPD socket")?,
    };
    match has_socket(owner, inode) {
        Ok(true) => Ok(()),
        Ok(false) => bail!("USB DAC is held by MPD PID {owner}, not the selected local MPD socket"),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied && enabled_alsa => {
            // A desktop user normally cannot read another user's fd links. Require
            // an exclusive service UID corroborated by an enabled MPD ALSA output.
            verify_exclusive_uid(owner, uid)
        }
        Err(err) => Err(err).with_context(|| format!("Cannot verify the selected MPD socket belongs to USB DAC owner PID {owner}")),
    }
}

fn ipv6_endpoint(ip: IpAddr, port: u16) -> String {
    let ip = match ip { IpAddr::V4(ip) => ip.to_ipv6_mapped(), IpAddr::V6(ip) => ip };
    let [a, b, c, d, e, f, g, h] = ip.segments();
    // procfs prints each 32-bit word in native byte order.
    let word = |hi: u16, lo: u16| ((u32::from(hi) << 16) | u32::from(lo)).to_be();
    format!("{:08X}{:08X}{:08X}{:08X}:{port:04X}", word(a, b), word(c, d), word(e, f), word(g, h))
}

fn socket_entry(path: &str, local: &str, peer: &str) -> Result<Option<(u32, u64)>> {
    let sockets = fs::read_to_string(path).with_context(|| format!("Reading local socket ownership from {path}"))?;
    for line in sockets.lines().skip(1) {
        let mut columns = line.split_ascii_whitespace().skip(1);
        if columns.next() != Some(local) || columns.next() != Some(peer) || columns.next() != Some("01") { continue; }
        let uid = columns.nth(3).and_then(|value| value.parse().ok()).context("Unknown selected MPD socket UID")?;
        let inode = columns.nth(1).and_then(|value| value.parse().ok()).filter(|&inode| inode != 0)
            .context("Unknown selected MPD socket inode")?;
        return Ok(Some((uid, inode)));
    }
    Ok(None)
}

fn has_socket(owner: u32, inode: u64) -> io::Result<bool> {
    let socket = format!("socket:[{inode}]");
    for entry in fs::read_dir(format!("/proc/{owner}/fd"))? {
        match fs::read_link(entry?.path()) {
            Ok(target) if target == Path::new(&socket) => return Ok(true),
            Ok(_) => (),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

fn verify_exclusive_uid(owner: u32, uid: u32) -> Result<()> {
    let fs_uid = |status: &str| field(status, "Uid").and_then(|uids| uids.split_ascii_whitespace().nth(3))
        .and_then(|uid| uid.parse::<u32>().ok()).context("Cannot identify process filesystem UID");
    let status = fs::read_to_string(format!("/proc/{owner}/status"))?;
    ensure!(fs_uid(&status)? == uid, "USB DAC owner and selected local MPD socket have different service UIDs");
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(positive_u32) else { continue };
        if pid == owner { continue; }
        let status = match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("Cannot exclude a different owner of the selected MPD socket (PID {pid})")),
        };
        ensure!(fs_uid(&status)? != uid, "Cannot verify which process owns the selected MPD socket: service UID {uid} is shared");
    }
    Ok(())
}
