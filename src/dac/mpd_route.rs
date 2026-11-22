use std::{fs::{self, File}, io::Read, path::Path};

const MOODE_ALSA_ROUTE: &str = "/etc/alsa/conf.d/_audioout.conf";

pub(super) fn direct(owner: u32, output: &str, card: u32, device: u32, id: &str) -> bool {
    let Some(config) = mpd_config(owner) else { return false; };
    let Some(route) = fixed_alsa_device(&config, output) else { return false; };
    if route == "_audioout" {
        read_config(Path::new(MOODE_ALSA_ROUTE)).as_deref().and_then(moode_alsa_slave)
            .is_some_and(|slave| hardware_matches(slave, card, device, id))
    } else {
        hardware_matches(route, card, device, id)
    }
}

fn mpd_config(owner: u32) -> Option<String> {
    let cmdline = read_config(Path::new(&format!("/proc/{owner}/cmdline")))?;
    let mut args = cmdline.split_terminator('\0');
    if Path::new(args.next()?).file_name()? != "mpd" { return None; }
    let config = args.find(|arg| !matches!(*arg, "--systemd" | "--no-daemon"))?;
    let path = Path::new(config);
    if !path.is_absolute() || args.next().is_some() { return None; }
    read_config(path)
}

fn read_config(path: &Path) -> Option<String> {
    const LIMIT: u64 = 64 * 1024;
    if !fs::metadata(path).ok()?.is_file() { return None; }
    let mut buffer = String::new();
    File::open(path).ok()?.take(LIMIT + 1).read_to_string(&mut buffer).ok()?;
    (buffer.len() as u64 <= LIMIT).then_some(buffer)
}

fn lines(config: &str) -> impl Iterator<Item = &str> {
    config.lines().map(|line| line.split('#').next().unwrap_or_default().trim()).filter(|line| !line.is_empty())
}

fn directive(line: &str) -> (&str, &str) {
    let (key, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    (key, value.trim().trim_matches('"'))
}

fn fixed_alsa_device<'a>(config: &'a str, output: &str) -> Option<&'a str> {
    let mut lines = lines(config);
    let mut selected = None;
    while let Some(line) = lines.next() {
        let (key, value) = directive(line);
        if matches!(key, "include" | "include_optional" | "audio_output_format")
            || (key == "volume_normalization" && value != "no") { return None; }
        if value != "{" { continue; }
        let block = lines.by_ref().take_while(|line| *line != "}");
        if key != "audio_output" { block.for_each(drop); continue; }
        let (mut kind, mut name, mut device, mut mixer) = ("", "", "", "");
        let mut direct = true;
        for (key, value) in block.map(directive) {
            match key {
                "type" => kind = value,
                "name" => name = value,
                "device" => device = value,
                "mixer_type" => mixer = value,
                "dop" if value == "no" => (),
                "close_on_pause" | "stop_dsd_silence" | "thesycon_dsd_workaround" => (),
                // An unrecognized option (e.g. format or filters) is not evidence of a direct path.
                _ => direct = false,
            }
        }
        if name == output {
            if selected.is_some() || !direct || kind != "alsa" || mixer != "none" || device.is_empty() { return None; }
            selected = Some(device);
        }
    }
    selected
}

fn moode_alsa_slave(config: &str) -> Option<&str> {
    let mut lines = lines(config);
    if directive(lines.next()?) != ("pcm._audioout", "{") || directive(lines.next()?) != ("type", "copy") { return None; }
    let (key, slave) = directive(lines.next()?);
    (key == "slave.pcm" && lines.next() == Some("}") && lines.next().is_none()).then_some(slave)
}

fn hardware_matches(route: &str, card: u32, device: u32, id: &str) -> bool {
    let Some((plugin, address)) = route.split_once(':') else { return false; };
    if !matches!(plugin, "hw" | "plughw") { return false; }
    let Some((card_ref, dev_ref)) = address.split_once(',') else { return false; };
    let card_ref = card_ref.strip_prefix("CARD=").unwrap_or(card_ref);
    let dev_ref = dev_ref.strip_prefix("DEV=").unwrap_or(dev_ref);
    (card_ref == id || card_ref.parse::<u32>() == Ok(card)) && dev_ref.parse::<u32>() == Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_mode_requires_the_active_output_to_bypass_audio_filters() {
        let config = "volume_normalization \"no\"\naudio_output {\ntype \"alsa\"\nname \"DAC\"\ndevice \"_audioout\"\nmixer_type \"none\"\n}\n";
        assert_eq!(fixed_alsa_device(config, "DAC"), Some("_audioout"));
        assert_eq!(fixed_alsa_device(config, "Other DAC"), None);
        assert_eq!(fixed_alsa_device(&config.replace("mixer_type \"none\"", "mixer_type \"software\""), "DAC"), None);
        assert_eq!(fixed_alsa_device(&config.replace("\n}", "\nfilters \"equalizer\"\n}"), "DAC"), None);
        assert_eq!(fixed_alsa_device(&config.replace("\"no\"", "\"yes\""), "DAC"), None);
    }

    #[test]
    fn direct_alias_must_reach_the_observed_usb_pcm_without_dsp() {
        let alias = "pcm._audioout {\ntype copy\nslave.pcm \"plughw:1,0\"\n}";
        let slave = moode_alsa_slave(alias).unwrap();
        assert!(hardware_matches(slave, 1, 0, "K17"));
        assert!(!hardware_matches(slave, 2, 0, "K17"));
        assert!(hardware_matches("hw:CARD=K17,DEV=0", 1, 0, "K17"));
        assert_eq!(moode_alsa_slave(&alias.replace("type copy", "type equal")), None);
    }
}
