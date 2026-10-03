//! The drop-in voice/video lounge on top of the mesh: who is in voice, which media each
//! link carries, the voice/video caps and the speaking meter.
//!
//! Each link carries at most one audio and one video sender. They are created with
//! `addTrack` the first time media is needed and switched with `replaceTrack` afterwards
//! (including `None` when the camera goes off or someone leaves voice), so toggles do not
//! renegotiate or grow the SDP.

use super::RoomSession;
use crate::media::{self, SpeakingMeter};
use crate::mesh::PeerLink;
use crate::names::pubkey_tag;
use crate::state::{AudioSettings, LoungeMemberUi, MyVoiceUi};
use leptos::*;
use protocol::{latest_beyond_cap, RoomBody, RoomEnvelope, VideoKind};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, MediaStream, MediaStreamTrack, RtcRtpSender};

const METER_INTERVAL_MS: i32 = 150;

/// A member's latest lounge state, from their signed `VoiceState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct VoiceInfo {
    pub seq: u64,
    pub in_voice: bool,
    pub voice_ts: u64,
    pub mic_muted: bool,
    pub video: VideoKind,
    pub video_ts: u64,
}

#[derive(Default)]
struct LinkSenders {
    audio: Option<RtcRtpSender>,
    video: Option<RtcRtpSender>,
}

pub(super) struct Lounge {
    joining: Cell<bool>,
    speaker_muted: Cell<bool>,
    front_camera: Cell<bool>,
    /// The mic (and video) tracks this tab sends, in one stream so peers get them together.
    local_stream: RefCell<Option<MediaStream>>,
    audio_settings: Cell<AudioSettings>,
    recapture_in_flight: Cell<bool>,
    pending_settings: Cell<Option<AudioSettings>>,
    senders: RefCell<HashMap<String, LinkSenders>>,
    remote_streams: RefCell<HashMap<String, MediaStream>>,
    states: RefCell<HashMap<String, VoiceInfo>>,
    /// Latest signed `VoiceState` per member, replayed to new neighbors.
    pub envelopes: RefCell<HashMap<String, RoomEnvelope>>,
    seq: Cell<u64>,
    meter: RefCell<Option<SpeakingMeter>>,
    speaking: RefCell<Vec<String>>,
    /// Members over the voice / video cap as of the last recompute.
    voice_evicted: RefCell<HashSet<String>>,
    video_evicted: RefCell<HashSet<String>>,
}

impl Default for Lounge {
    fn default() -> Self {
        Self {
            joining: Cell::new(false),
            speaker_muted: Cell::new(false),
            front_camera: Cell::new(true),
            local_stream: RefCell::new(None),
            audio_settings: Cell::new(AudioSettings::default()),
            recapture_in_flight: Cell::new(false),
            pending_settings: Cell::new(None),
            senders: RefCell::new(HashMap::new()),
            remote_streams: RefCell::new(HashMap::new()),
            states: RefCell::new(HashMap::new()),
            envelopes: RefCell::new(HashMap::new()),
            seq: Cell::new(0),
            meter: RefCell::new(None),
            speaking: RefCell::new(Vec::new()),
            voice_evicted: RefCell::new(HashSet::new()),
            video_evicted: RefCell::new(HashSet::new()),
        }
    }
}

impl RoomSession {
    // ---- Controls ----------------------------------------------------------------------

    /// Capture the mic and enter the lounge. Call from a user gesture (it also starts
    /// the Web Audio meter and unlocks remote audio playback).
    pub fn join_voice(&self) {
        let lounge = &self.inner.lounge;
        if self.my_voice().in_voice || lounge.joining.get() || self.inner.closed.get() {
            return;
        }
        if self.voice_slots_full() {
            self.toast("voice_full");
            return;
        }
        lounge.joining.set(true);
        self.refresh_my_voice();
        media::resume_remote_audio();
        if lounge.meter.borrow().is_none() {
            match SpeakingMeter::new() {
                Ok(meter) => {
                    *lounge.meter.borrow_mut() = Some(meter);
                    self.start_meter();
                }
                Err(err) => log::warn!("Speaking meter unavailable: {:?}", err),
            }
        }

        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let settings = s.inner.lounge.audio_settings.get();
            let captured = media::capture_microphone(&settings).await;
            s.inner.lounge.joining.set(false);
            let track = match captured {
                Ok(track) if !s.inner.closed.get() => track,
                Ok(track) => {
                    track.stop();
                    return;
                }
                Err(err) => {
                    log::error!("Microphone capture failed: {:?}", err);
                    s.toast("toast_mic_denied");
                    s.refresh_my_voice();
                    return;
                }
            };
            let Ok(stream) = MediaStream::new() else {
                track.stop();
                return;
            };
            stream.add_track(&track);
            *s.inner.lounge.local_stream.borrow_mut() = Some(stream);
            s.watch_local_mic();
            let now = js_sys::Date::now() as u64;
            s.publish_voice(|v| {
                v.in_voice = true;
                v.voice_ts = now;
                v.mic_muted = false;
                v.video = VideoKind::None;
                v.video_ts = 0;
            });
        });
    }

    pub fn leave_voice(&self) {
        if !self.my_voice().in_voice {
            return;
        }
        let lounge = &self.inner.lounge;
        if let Some(stream) = lounge.local_stream.borrow_mut().take() {
            for track in stream.get_tracks().iter() {
                track.unchecked_into::<MediaStreamTrack>().stop();
            }
        }
        if let Some(meter) = lounge.meter.borrow_mut().as_mut() {
            meter.unwatch(&self.inner.me);
        }
        if lounge.speaker_muted.replace(false) {
            media::set_remote_audio_muted(false);
        }
        self.publish_voice(|v| {
            v.in_voice = false;
            v.mic_muted = false;
            v.video = VideoKind::None;
            v.video_ts = 0;
        });
    }

    pub fn toggle_mic(&self) {
        let mine = self.my_voice();
        if !mine.in_voice {
            return;
        }
        let muted = !mine.mic_muted;
        if let Some(track) = self.local_track("audio") {
            track.set_enabled(!muted);
        }
        self.publish_voice(|v| v.mic_muted = muted);
    }

    /// Mute every member's incoming audio locally. Peers are not notified.
    pub fn toggle_speaker(&self) {
        let lounge = &self.inner.lounge;
        let muted = !lounge.speaker_muted.get();
        lounge.speaker_muted.set(muted);
        media::set_remote_audio_muted(muted);
        self.refresh_my_voice();
    }

    pub fn toggle_camera(&self) {
        let mine = self.my_voice();
        if !mine.in_voice {
            return;
        }
        if mine.video == VideoKind::Camera {
            self.stop_video();
            return;
        }
        if mine.video == VideoKind::None && self.video_slots_full() {
            self.toast("video_full_title");
            return;
        }
        let s = self.clone();
        let front = self.inner.lounge.front_camera.get();
        wasm_bindgen_futures::spawn_local(async move {
            match media::capture_camera(front).await {
                Ok(track) => s.set_video_track(track, VideoKind::Camera),
                Err(err) => {
                    log::error!("Camera capture failed: {:?}", err);
                    s.toast("toast_camera_denied");
                }
            }
        });
    }

    pub fn toggle_screen(&self) {
        let mine = self.my_voice();
        if !mine.in_voice {
            return;
        }
        if mine.video == VideoKind::Screen {
            self.stop_video();
            return;
        }
        if mine.video == VideoKind::None && self.video_slots_full() {
            self.toast("video_full_title");
            return;
        }
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match media::capture_screen().await {
                Ok(track) => {
                    // Stopping from the browser's own "Stop sharing" control ends the share.
                    let s_end = s.clone();
                    let track_id = track.id();
                    let on_ended = Closure::wrap(Box::new(move || {
                        if s_end.local_track("video").is_some_and(|t| t.id() == track_id) {
                            s_end.stop_video();
                        }
                    }) as Box<dyn FnMut()>);
                    track.set_onended(Some(on_ended.as_ref().unchecked_ref()));
                    on_ended.forget();
                    s.set_video_track(track, VideoKind::Screen);
                }
                Err(err) => {
                    log::error!("Screen capture failed: {:?}", err);
                    s.toast("toast_screen_denied");
                }
            }
        });
    }

    /// Switch between front and rear camera; peers keep their sender (no renegotiation).
    pub fn flip_camera(&self) {
        if self.my_voice().video != VideoKind::Camera {
            return;
        }
        let lounge = &self.inner.lounge;
        let front = !lounge.front_camera.get();
        lounge.front_camera.set(front);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match media::capture_camera(front).await {
                Ok(track) => {
                    if s.my_voice().video != VideoKind::Camera || !s.replace_local_track("video", &track) {
                        track.stop();
                        return;
                    }
                    s.sync_media_all();
                    s.attach_lounge_media();
                }
                Err(err) => {
                    log::error!("Camera flip failed: {:?}", err);
                    s.toast("toast_camera_switch_failed");
                }
            }
        });
    }

    /// Store new mic processing settings and, in voice, swap in a freshly captured mic
    /// track. Changes arriving mid-swap collapse into one follow-up swap (latest wins).
    pub fn set_audio_settings(&self, settings: AudioSettings) {
        let lounge = &self.inner.lounge;
        lounge.audio_settings.set(settings);
        if lounge.recapture_in_flight.get() {
            lounge.pending_settings.set(Some(settings));
            return;
        }
        if self.local_track("audio").is_none() {
            return;
        }
        lounge.recapture_in_flight.set(true);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut next = Some(settings);
            while let Some(settings) = next {
                if let Err(err) = s.swap_mic(&settings).await {
                    log::error!("Audio settings re-capture error: {:?}", err);
                    s.toast("toast_audio_settings_failed");
                }
                next = s.inner.lounge.pending_settings.take();
            }
            s.inner.lounge.recapture_in_flight.set(false);
        });
    }

    pub fn voice_cap(&self) -> Option<usize> {
        self.inner.params.voice_cap
    }

    pub fn video_cap(&self) -> Option<usize> {
        self.inner.params.video_cap
    }

    // ---- State -------------------------------------------------------------------------

    pub(super) fn my_voice(&self) -> VoiceInfo {
        self.voice_of(&self.inner.me)
    }

    fn voice_of(&self, pubkey: &str) -> VoiceInfo {
        self.inner.lounge.states.borrow().get(pubkey).copied().unwrap_or_default()
    }

    /// Sign and gossip this tab's lounge state after applying `update`.
    fn publish_voice(&self, update: impl FnOnce(&mut VoiceInfo)) {
        let mut info = self.my_voice();
        update(&mut info);
        let seq = self.inner.lounge.seq.get() + 1;
        self.inner.lounge.seq.set(seq);
        self.publish(RoomBody::VoiceState {
            seq,
            in_voice: info.in_voice,
            voice_ts: info.voice_ts,
            mic_muted: info.mic_muted,
            video: info.video,
            video_ts: info.video_ts,
        });
    }

    /// A member's `VoiceState` (including our own, via `publish`).
    pub(super) fn on_voice_state(&self, envelope: &RoomEnvelope, info: VoiceInfo) {
        let author = envelope.author.as_str();
        let lounge = &self.inner.lounge;
        let previous = lounge.states.borrow().get(author).copied();
        if previous.is_some_and(|p| p.seq >= info.seq) {
            return;
        }
        lounge.states.borrow_mut().insert(author.to_string(), info);
        lounge.envelopes.borrow_mut().insert(author.to_string(), envelope.clone());

        let newly_in_voice = info.in_voice && !previous.is_some_and(|p| p.in_voice);
        if newly_in_voice
            && author != self.inner.me
            && !self.my_voice().in_voice
            && info.voice_ts > self.inner.join_ts
        {
            if let Some(name) = self.inner.names.borrow().get(author) {
                self.inner.signals.voice_prompt.set(Some(name.clone()));
            }
        }
        self.recompute_lounge();
    }

    /// Enforce the voice/video caps, align every link's media with who is in voice, and
    /// refresh the lounge UI. Called on any roster, link or voice change.
    pub(super) fn recompute_lounge(&self) {
        if self.inner.closed.get() {
            return;
        }
        let me = self.inner.me.clone();
        let present = self.inner.present.borrow().clone();
        let (voice_evicted, video_evicted) = {
            let states = self.inner.lounge.states.borrow();
            let in_voice: Vec<(String, u64)> = states
                .iter()
                .filter(|(pk, v)| v.in_voice && present.contains(*pk))
                .map(|(pk, v)| (pk.clone(), v.voice_ts))
                .collect();
            let voice_evicted: HashSet<String> =
                latest_beyond_cap(in_voice.iter().cloned(), self.inner.params.voice_cap).into_iter().collect();
            let with_video = in_voice
                .iter()
                .filter(|(pk, _)| !voice_evicted.contains(pk))
                .filter_map(|(pk, _)| {
                    let v = states.get(pk)?;
                    (v.video != VideoKind::None).then(|| (pk.clone(), v.video_ts))
                });
            let video_evicted: HashSet<String> =
                latest_beyond_cap(with_video, self.inner.params.video_cap).into_iter().collect();
            (voice_evicted, video_evicted)
        };
        let me_voice_evicted = voice_evicted.contains(&me);
        let me_video_evicted = video_evicted.contains(&me);
        *self.inner.lounge.voice_evicted.borrow_mut() = voice_evicted;
        *self.inner.lounge.video_evicted.borrow_mut() = video_evicted;

        if me_voice_evicted {
            self.toast("toast_voice_full");
            self.leave_voice();
            return;
        }
        if me_video_evicted {
            self.toast("toast_video_full");
            self.stop_video();
            return;
        }
        self.sync_media_all();
        self.update_lounge_ui();
    }

    fn in_voice_seat(&self, pubkey: &str) -> bool {
        self.voice_of(pubkey).in_voice && !self.inner.lounge.voice_evicted.borrow().contains(pubkey)
    }

    fn voice_slots_full(&self) -> bool {
        let Some(cap) = self.inner.params.voice_cap else {
            return false;
        };
        let present = self.inner.present.borrow();
        let taken = present
            .iter()
            .filter(|pk| **pk != self.inner.me && self.in_voice_seat(pk))
            .count();
        taken >= cap
    }

    fn video_slots_full(&self) -> bool {
        let Some(cap) = self.inner.params.video_cap else {
            return false;
        };
        let video_evicted = self.inner.lounge.video_evicted.borrow();
        let present = self.inner.present.borrow();
        let taken = present
            .iter()
            .filter(|pk| **pk != self.inner.me && self.in_voice_seat(pk))
            .filter(|pk| self.voice_of(pk).video != VideoKind::None && !video_evicted.contains(*pk))
            .count();
        taken >= cap
    }

    // ---- Media plumbing ----------------------------------------------------------------

    fn local_track(&self, kind: &str) -> Option<MediaStreamTrack> {
        self.inner.lounge.local_stream.borrow().as_ref().and_then(|s| media::first_track(s, kind))
    }

    /// Swap the local track of `kind` for `track`; `false` if not in voice anymore.
    fn replace_local_track(&self, kind: &str, track: &MediaStreamTrack) -> bool {
        let lounge = &self.inner.lounge;
        let stream = lounge.local_stream.borrow();
        let Some(stream) = stream.as_ref() else {
            return false;
        };
        if let Some(old) = media::first_track(stream, kind) {
            stream.remove_track(&old);
            old.stop();
        }
        stream.add_track(track);
        true
    }

    fn set_video_track(&self, track: MediaStreamTrack, kind: VideoKind) {
        if !self.my_voice().in_voice || !self.replace_local_track("video", &track) {
            track.stop();
            return;
        }
        let had_video = self.my_voice().video != VideoKind::None;
        let now = js_sys::Date::now() as u64;
        self.publish_voice(|v| {
            v.video = kind;
            if !had_video {
                v.video_ts = now;
            }
        });
    }

    fn stop_video(&self) {
        if let Some(stream) = self.inner.lounge.local_stream.borrow().as_ref() {
            if let Some(track) = media::first_track(stream, "video") {
                stream.remove_track(&track);
                track.stop();
            }
        }
        self.publish_voice(|v| {
            v.video = VideoKind::None;
            v.video_ts = 0;
        });
    }

    async fn swap_mic(&self, settings: &AudioSettings) -> Result<(), wasm_bindgen::JsValue> {
        let Some(old) = self.local_track("audio") else {
            // Left voice while a previous swap was in flight.
            return Ok(());
        };
        let was_enabled = old.enabled();
        // Chrome hands a new capture the processing of an already-open mic source,
        // so the old track must be stopped first; peers hear a short gap.
        old.stop();
        let new_track = media::capture_microphone(settings).await?;
        new_track.set_enabled(was_enabled);
        if !self.replace_local_track("audio", &new_track) {
            new_track.stop();
            return Ok(());
        }
        self.sync_media_all();
        self.watch_local_mic();
        Ok(())
    }

    pub(super) fn sync_media_all(&self) {
        let links: Vec<(String, Rc<PeerLink>)> = self
            .inner
            .links
            .borrow()
            .iter()
            .map(|(remote, link)| (remote.clone(), link.clone()))
            .collect();
        for (remote, link) in links {
            self.sync_media_for(&remote, &link);
        }
    }

    /// Send our mic/video to `remote` exactly when both of us hold a voice seat and the
    /// link is open; otherwise send nothing.
    pub(super) fn sync_media_for(&self, remote: &str, link: &PeerLink) {
        let me = self.inner.me.clone();
        let wanted = link.is_open() && self.in_voice_seat(&me) && self.in_voice_seat(remote);
        let stream = self.inner.lounge.local_stream.borrow().clone();
        let video_allowed = self.my_voice().video != VideoKind::None
            && !self.inner.lounge.video_evicted.borrow().contains(&me);
        let (audio, video) = match (&stream, wanted) {
            (Some(stream), true) => (
                media::first_track(stream, "audio"),
                media::first_track(stream, "video").filter(|_| video_allowed),
            ),
            _ => (None, None),
        };
        let mut senders = self.inner.lounge.senders.borrow_mut();
        let entry = senders.entry(remote.to_string()).or_default();
        entry.audio = reconcile_sender(link, entry.audio.take(), audio, stream.as_ref());
        entry.video = reconcile_sender(link, entry.video.take(), video, stream.as_ref());
    }

    pub(super) fn on_remote_track(&self, remote: &str, track: MediaStreamTrack, stream: Option<MediaStream>) {
        let lounge = &self.inner.lounge;
        let target = {
            let mut streams = lounge.remote_streams.borrow_mut();
            if !streams.contains_key(remote) {
                let Some(fresh) = stream.or_else(|| MediaStream::new().ok()) else {
                    return;
                };
                streams.insert(remote.to_string(), fresh);
            }
            streams[remote].clone()
        };
        let known = target.get_tracks().iter().any(|t| t.unchecked_into::<MediaStreamTrack>().id() == track.id());
        if !known {
            target.add_track(&track);
        }
        if track.kind() == "audio" {
            if let Some(promise) = media::play_remote_audio(remote, &target, lounge.speaker_muted.get()) {
                let s = self.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if JsFuture::from(promise).await.is_err() {
                        s.toast("toast_tap_audio");
                    }
                });
            }
            if let Some(meter) = lounge.meter.borrow_mut().as_mut() {
                meter.watch(remote, &target);
            }
        }
        self.attach_lounge_media();
    }

    /// Drop everything media-related for `remote` (link gone). `forget_state` also forgets
    /// their lounge state (they left the room).
    pub(super) fn forget_member_media(&self, remote: &str, forget_state: bool) {
        let lounge = &self.inner.lounge;
        lounge.senders.borrow_mut().remove(remote);
        lounge.remote_streams.borrow_mut().remove(remote);
        media::remove_remote_audio(remote);
        if let Some(meter) = lounge.meter.borrow_mut().as_mut() {
            meter.unwatch(remote);
        }
        if forget_state {
            lounge.states.borrow_mut().remove(remote);
            lounge.envelopes.borrow_mut().remove(remote);
        }
    }

    // ---- UI ----------------------------------------------------------------------------

    fn update_lounge_ui(&self) {
        let me = self.inner.me.clone();
        let present = self.inner.present.borrow().clone();
        let names = self.inner.names.borrow();
        let links = self.inner.links.borrow();
        let video_evicted = self.inner.lounge.video_evicted.borrow();
        let states = self.inner.lounge.states.borrow();
        let mut seated: Vec<(&String, &VoiceInfo)> = states
            .iter()
            .filter(|(pk, _)| present.contains(*pk) && self.in_voice_seat(pk))
            .collect();
        seated.sort_by(|a, b| (a.1.voice_ts, a.0).cmp(&(b.1.voice_ts, b.0)));
        let members: Vec<LoungeMemberUi> = seated
            .into_iter()
            .map(|(pk, v)| LoungeMemberUi {
                pubkey: pk.clone(),
                name: names.get(pk).cloned().unwrap_or_else(|| pubkey_tag(pk)),
                tag: pubkey_tag(pk),
                is_self: *pk == me,
                mic_muted: v.mic_muted,
                video: if video_evicted.contains(pk) { VideoKind::None } else { v.video },
                has_media_link: *pk == me || links.get(pk).is_some_and(|l| l.is_open()),
            })
            .collect();
        drop((names, links, video_evicted, states));
        self.inner.signals.lounge.set(members);
        self.refresh_my_voice();
        self.attach_lounge_media();
    }

    fn refresh_my_voice(&self) {
        let mine = self.my_voice();
        let lounge = &self.inner.lounge;
        self.inner.signals.my_voice.set(MyVoiceUi {
            in_voice: mine.in_voice,
            joining: lounge.joining.get(),
            mic_muted: mine.mic_muted,
            speaker_muted: lounge.speaker_muted.get(),
            video: mine.video,
        });
    }

    /// Point every visible video tile at its stream (tiles re-render as the lounge changes).
    pub fn attach_lounge_media(&self) {
        let me = &self.inner.me;
        let lounge = &self.inner.lounge;
        let states = lounge.states.borrow();
        for (pk, info) in states.iter() {
            if info.video == VideoKind::None || !info.in_voice {
                continue;
            }
            let stream = if pk == me {
                lounge.local_stream.borrow().clone()
            } else {
                lounge.remote_streams.borrow().get(pk).cloned()
            };
            if let Some(stream) = stream {
                media::attach_video(format!("tile-video-{pk}"), &stream);
            }
        }
    }

    fn watch_local_mic(&self) {
        let lounge = &self.inner.lounge;
        let stream = lounge.local_stream.borrow().clone();
        if let (Some(meter), Some(stream)) = (lounge.meter.borrow_mut().as_mut(), stream) {
            meter.watch(&self.inner.me, &stream);
        }
    }

    fn start_meter(&self) {
        let s = self.clone();
        let cb = Closure::wrap(Box::new(move || {
            if s.inner.closed.get() {
                return;
            }
            let lounge = &s.inner.lounge;
            let Some(mut speaking) = lounge.meter.borrow_mut().as_mut().map(|m| m.sample(js_sys::Date::now())) else {
                return;
            };
            speaking.retain(|pk| {
                let v = s.voice_of(pk);
                v.in_voice && !v.mic_muted
            });
            speaking.sort();
            if *lounge.speaking.borrow() != speaking {
                *lounge.speaking.borrow_mut() = speaking.clone();
                s.inner.signals.speaking.set(speaking.into_iter().collect());
            }
        }) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            let _ = w.set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), METER_INTERVAL_MS);
        }
        cb.forget();
    }

    pub(super) fn close_lounge(&self) {
        self.leave_voice();
        if let Some(mut meter) = self.inner.lounge.meter.borrow_mut().take() {
            meter.close();
        }
    }
}

/// Bring one sender in line with the wanted track: add it the first time (renegotiates),
/// then only `replaceTrack` (no renegotiation), including `None` to stop sending.
fn reconcile_sender(
    link: &PeerLink,
    sender: Option<RtcRtpSender>,
    track: Option<MediaStreamTrack>,
    stream: Option<&MediaStream>,
) -> Option<RtcRtpSender> {
    match (sender, track) {
        (None, None) => None,
        (None, Some(track)) => stream.map(|stream| link.add_track(&track, stream)),
        (Some(sender), track) => {
            if sender.track().map(|t| t.id()) != track.as_ref().map(|t| t.id()) {
                let promise = sender.replace_track(track.as_ref());
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(err) = JsFuture::from(promise).await {
                        log::warn!("replaceTrack failed: {:?}", err);
                    }
                });
            }
            Some(sender)
        }
    }
}
