use super::{Api, Event, PageItems, PlayerState, Queue, Renderer, Track, UsbPlay, catalog};
use crate::model::QueuePlacement;
use anyhow::{Context, Result, bail, ensure};
use std::{sync::mpsc::TryRecvError, time::{Duration, Instant}};

const TTL: Duration = Duration::from_secs(30);

pub(super) struct Stage {
    pub(super) request: u64,
    deadline: Instant,
    device: String,
    preparation: Preparation,
}

enum Preparation {
    Catalog(catalog::PendingQueue),
    Ready(Ready),
}

struct Ready {
    output: OutputGuard,
    play: Play,
}

enum Play {
    Player { target: String, use_current: bool },
    Catalog(catalog::PreparedQueue),
}

enum PlayerTarget<'a> { Current(&'a str), Queue(String) }

struct OutputGuard {
    renderer: String,
    revision: u64,
    selection: Selection,
}

#[derive(PartialEq, Eq)]
enum Selection { AlreadyLocal, Select, Fallback }

impl Stage {
    pub(super) fn begin(request: u64, device: String, play: UsbPlay, api: &Api, reads: &catalog::Reader) -> Result<Self> {
        let deadline = Instant::now() + TTL;
        api.check_usb_device(&device)?;
        let preparation = match play {
            UsbPlay::Player(entry) => {
                let (player, selection) = api.usb_player_selection(&device)?;
                let target = api.usb_player_target(&player, entry.as_deref())?;
                // Preserve generic resume for the current entry, including a detached cursor.
                // When starting the queue, bind Play to the first entry we actually checked.
                let play = match target {
                    PlayerTarget::Current(_) => Play::Player { target: player.current_entry_id, use_current: true },
                    PlayerTarget::Queue(id) => Play::Player { target: id, use_current: false },
                };
                let output = OutputGuard { renderer: player.renderer_id, revision: player.revision, selection };
                Preparation::Ready(Ready { output, play })
            }
            UsbPlay::Catalog(target) => Preparation::Catalog(reads.prepare(target, catalog::QueuePurpose::UsbPlay)?),
        };
        ensure!(Instant::now() < deadline, "USB Play preparation expired; try again");
        Ok(Self { request, deadline, device, preparation })
    }

    pub(super) fn awaiting_catalog(&self) -> bool { matches!(self.preparation, Preparation::Catalog(_)) }

    pub(super) fn expired(&self) -> bool { Instant::now() >= self.deadline }

    pub(super) fn wait(&self, wait: Duration) -> Duration {
        let wait = wait.min(self.deadline.saturating_duration_since(Instant::now()));
        if self.awaiting_catalog() { wait.min(Duration::from_millis(25)) } else { wait }
    }

    pub(super) fn poll_catalog(&mut self, api: Option<&Api>) -> Option<Result<()>> {
        let Preparation::Catalog(pending) = &self.preparation else { return None; };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("catalog worker stopped")),
        };
        Some((|| {
            let prepared = result?;
            let api = api.context("jastreamer session changed")?;
            api.check_usb_device(&self.device)?;
            let (player, selection) = api.usb_player_selection(&self.device)?;
            let output = OutputGuard { renderer: player.renderer_id, revision: player.revision, selection };
            ensure!(!self.expired(), "USB Play preparation expired; try again");
            self.preparation = Preparation::Ready(Ready { output, play: Play::Catalog(prepared) });
            Ok(())
        })())
    }

    pub(super) fn commit(self, api: &mut Api, emit: &impl Fn(Event)) -> Result<()> {
        let Self { device, preparation, deadline, .. } = self;
        let Preparation::Ready(Ready { output, play }) = preparation else { bail!("USB Play was not prepared"); };
        api.check_usb_device(&device)?;
        if let Play::Catalog(prepared) = &play { api.check_usb_catalog(prepared)?; }
        let (player, selection) = api.usb_player_selection(&device)?;
        ensure!(output.renderer == player.renderer_id && output.revision == player.revision && output.selection == selection,
            "the selected server output changed; try USB Play again");
        if let Play::Player { target, use_current } = &play {
            let verified = api.usb_player_target(&player, (!*use_current).then_some(target.as_str()))?;
            let verified = match &verified {
                PlayerTarget::Current(id) => *id,
                PlayerTarget::Queue(id) => id.as_str(),
            };
            ensure!(target.as_str() == verified, "the server queue changed; try USB Play again");
        }
        ensure!(Instant::now() < deadline, "USB Play preparation expired; try again");

        let attached = api.output.is_none();
        if attached { api.attach(device, emit)?; }
        let result = (|| {
            ensure!(Instant::now() < deadline, "USB Play preparation expired; try again");
            let renderer = api.registered_usb_renderer()?;
            match output.selection {
                Selection::AlreadyLocal => {},
                Selection::Select => api.select_usb(renderer)?,
                Selection::Fallback => api.fallback_usb(renderer, &output.renderer, output.revision)?,
            }
            ensure!(Instant::now() < deadline, "USB Play preparation expired; try again");
            match play {
                Play::Player { target, use_current } => api.player_command("play", (!use_current).then_some(target.as_str()), None),
                Play::Catalog(prepared) => api.catalog_queue(QueuePlacement::Play, prepared, emit).map(emit),
            }
        })();
        if result.is_err() && attached { api.release(emit); }
        result
    }
}

impl Api {
    fn check_usb_device(&self, device: &str) -> Result<()> {
        ensure!(self.cookie.is_some(), "sign in to play on the Pi USB DAC");
        ensure!(crate::dac::devices().iter().any(|found| found == device), "selected USB DAC is no longer connected");
        if let Some(local) = &self.output {
            ensure!(local.device == device, "release the other USB DAC before transferring playback");
            self.registered_usb_renderer()?;
        }
        Ok(())
    }

    fn usb_player_selection(&self, device: &str) -> Result<(PlayerState, Selection)> {
        let player: PlayerState = self.get("/api/v1/player")?;
        ensure!(player.pending_command.is_empty(), "a server playback command is pending; wait and try again");
        let selection = if self.output.as_ref().is_some_and(|local| local.device == device && local.renderer == player.renderer_id) {
            Selection::AlreadyLocal
        } else if player.state == "stopped" {
            Selection::Select
        } else if player.state == "unavailable" && !player.renderer_id.is_empty() {
            let renderers: PageItems<Renderer> = self.get("/api/v1/renderers")?;
            ensure!(!renderers.items.iter().any(|renderer| renderer.online && renderer.id == player.renderer_id),
                "the selected server output is still online; it won't be replaced");
            Selection::Fallback
        } else {
            bail!("another server output is active; this handoff won't stop it");
        };
        Ok((player, selection))
    }

    fn usb_player_target<'a>(&self, player: &'a PlayerState, entry: Option<&str>) -> Result<PlayerTarget<'a>> {
        if entry.is_none() && !player.current_entry_id.is_empty() && let Some(track) = &player.track {
            check_usb_track(track)?;
            return Ok(PlayerTarget::Current(&player.current_entry_id));
        }
        let queue: Queue = self.get("/api/v1/queue")?;
        let selected = match entry {
            Some(id) => queue.entries.into_iter().find(|entry| entry.id == id).context("the requested server queue entry no longer exists")?,
            None => queue.entries.into_iter().next().context("the server queue is empty")?,
        };
        check_usb_track(&selected.track)?;
        Ok(PlayerTarget::Queue(selected.id))
    }
}

pub(super) fn check_usb_track(track: &Track) -> Result<()> {
    ensure!(track.available, "the selected track is unavailable");
    ensure!(crate::dac::SourceCodec::from_track_format(&track.format).is_some(),
        "the Pi USB DAC cannot decode this format: {}", track.format);
    Ok(())
}
