//! Video quality presets. The member who shares a camera or a screen makes one choice for
//! everyone who watches, and this table maps it to capture constraints and encoder settings.
//!
//! The output is engine-neutral on purpose: today the client turns it into
//! `applyConstraints` on the track plus `RTCRtpSender.setParameters` on every link, and a
//! future single encoder (WebCodecs, Chrome's Encoded Source) can read the same table
//! through [`webcodecs_config`] instead of growing a second one.
//!
//! Presets are local only. They never go on the wire, so changing this table never needs a
//! `PROTOCOL_VERSION` bump and never splits a live room.

/// Screen-share presets, from the lowest delay to the sharpest text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScreenPreset {
    /// 720p at 60 fps: the lowest delay, for remote control and games.
    Fastest,
    /// 1080p at 60 fps: fluid motion at full HD, at twice the bitrate of Balanced.
    Smooth,
    /// 1080p at 30 fps, kept crisp: what most shares want.
    #[default]
    Balanced,
    /// The source's own resolution at 30 fps, tuned for detail.
    Sharp,
    /// The source's own resolution at 15 fps, tuned for text.
    Text,
}

impl ScreenPreset {
    /// In ladder order, as the pickers list them.
    pub const ALL: [ScreenPreset; 5] = [Self::Fastest, Self::Smooth, Self::Balanced, Self::Sharp, Self::Text];

    /// Stable id for element ids and translation keys.
    pub fn id(self) -> &'static str {
        match self {
            Self::Fastest => "fastest",
            Self::Smooth => "smooth",
            Self::Balanced => "balanced",
            Self::Sharp => "sharp",
            Self::Text => "text",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// Camera presets. Balanced is what browsers do on their own (about 640×480 at 30 fps).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CameraPreset {
    /// 720p at 60 fps.
    Smooth60,
    /// 640×480 at 30 fps with the browser's own encoder settings.
    #[default]
    Balanced,
    /// 720p at 30 fps.
    Hd,
    /// 1080p at 30 fps.
    FullHd,
    /// 640×360 at 15 fps at the bitrate floor, for slow or metered uplinks.
    DataSaver,
}

impl CameraPreset {
    /// In ladder order, as the pickers list them.
    pub const ALL: [CameraPreset; 5] = [Self::Smooth60, Self::Balanced, Self::Hd, Self::FullHd, Self::DataSaver];

    /// Stable id for element ids and translation keys.
    pub fn id(self) -> &'static str {
        match self {
            Self::Smooth60 => "smooth60",
            Self::Balanced => "balanced",
            Self::Hd => "hd",
            Self::FullHd => "fullhd",
            Self::DataSaver => "datasaver",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// The sharer's choice for each source. RAM only: it survives voice rejoins and rekeys and
/// resets on reload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct VideoPresets {
    pub camera: CameraPreset,
    pub screen: ScreenPreset,
}

/// What the encoder gives up first when the link or the CPU can't keep up
/// (`RTCRtpSendParameters.degradationPreference`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Degradation {
    /// Keep the frame rate and drop resolution: motion stays fluid.
    MaintainFramerate,
    /// Keep the resolution and drop frames: text stays readable.
    MaintainResolution,
    Balanced,
}

impl Degradation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaintainFramerate => "maintain-framerate",
            Self::MaintainResolution => "maintain-resolution",
            Self::Balanced => "balanced",
        }
    }
}

/// `MediaStreamTrack.contentHint`. Browsers read it when the track is attached to a sender,
/// so it has to be set before that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hint {
    /// Empty: the browser's own choice for the source (screencast for a screen).
    None,
    Motion,
    Detail,
    Text,
}

impl Hint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Motion => "motion",
            Self::Detail => "detail",
            Self::Text => "text",
        }
    }
}

/// What a preset wants from the codec, turned into a ladder by [`codec_ladder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodecIntent {
    /// The lowest encode delay: hardware H.265 or H.264, else VP8.
    Motion,
    /// Sharp text and lines at a low bitrate: AV1 or VP9 when they keep up, else Motion's.
    Detail,
    /// Hardware H.264 when there is one, otherwise whatever the link negotiated.
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodecKind {
    H265,
    H264,
    Av1,
    Vp9,
    Vp8,
}

impl CodecKind {
    const ALL: [CodecKind; 5] = [Self::H265, Self::H264, Self::Av1, Self::Vp9, Self::Vp8];

    /// As in `RTCRtpCodec.mimeType`.
    pub fn mime(self) -> &'static str {
        match self {
            Self::H265 => "video/H265",
            Self::H264 => "video/H264",
            Self::Av1 => "video/AV1",
            Self::Vp9 => "video/VP9",
            Self::Vp8 => "video/VP8",
        }
    }

    /// Case-insensitive, as MIME types are. Retransmission and FEC entries (`video/rtx`,
    /// `video/red`, `video/ulpfec`, `video/flexfec-03`) name no codec and return `None`.
    pub fn from_mime(mime: &str) -> Option<Self> {
        let mime = mime.trim();
        Self::ALL.into_iter().find(|k| k.mime().eq_ignore_ascii_case(mime))
    }
}

/// What to ask of the capture. Never a minimum frame rate: Chrome's zero-hertz screen
/// capture would then hold frames back, adding up to a frame of delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CaptureTarget {
    /// With `ideal = false` (screens) these are upper bounds, and `None` keeps the source
    /// size: a capture is never upscaled. With `ideal = true` (cameras) they are ideal values
    /// the camera gets as close to as it can.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Screens: both the ideal and the maximum. Cameras: the ideal.
    pub fps: u32,
    pub ideal: bool,
}

/// `maxBitrate` = encoded pixels per second × `bits_per_pixel`, clamped to the floor and
/// the cap. An explicit `maxBitrate` lifts the 2.5 Mbps default Chrome and Safari give every
/// stream above 960×540; Firefox only ever goes lower than its own table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BitrateRule {
    pub bits_per_pixel: f64,
    pub floor_bps: u32,
    pub cap_bps: u32,
}

/// One preset in engine-neutral form. Every `None` means "the browser's default": the
/// adapter deletes that key rather than leaving a value from an earlier preset behind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoTarget {
    pub capture: CaptureTarget,
    /// Largest size to encode; bigger frames are scaled down (`scaleResolutionDownBy`).
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_framerate: Option<u32>,
    pub bitrate: Option<BitrateRule>,
    pub degradation: Option<Degradation>,
    pub hint: Hint,
    pub codec_intent: CodecIntent,
}

/// Bitrate rules for screens and cameras. The numbers are tunable starting points.
const SCREEN_BITRATE: BitrateRule = BitrateRule { bits_per_pixel: 0.1, floor_bps: 300_000, cap_bps: 20_000_000 };
const CAMERA_BITRATE: BitrateRule = BitrateRule { bits_per_pixel: 0.08, floor_bps: 300_000, cap_bps: 6_000_000 };

/// A screen preset's settings. Balanced keeps the resolution rather than using the
/// `balanced` preference, which Chrome and Safari turn into maintain-resolution for screen
/// content anyway; only Fastest and Smooth trade resolution for frame rate.
pub fn screen_target(p: ScreenPreset) -> VideoTarget {
    use Degradation::{MaintainFramerate, MaintainResolution};
    let (size, fps, degradation, hint, codec_intent) = match p {
        ScreenPreset::Fastest => (Some((1280, 720)), 60, MaintainFramerate, Hint::Motion, CodecIntent::Motion),
        ScreenPreset::Smooth => (Some((1920, 1080)), 60, MaintainFramerate, Hint::Motion, CodecIntent::Motion),
        ScreenPreset::Balanced => (Some((1920, 1080)), 30, MaintainResolution, Hint::None, CodecIntent::Default),
        ScreenPreset::Sharp => (None, 30, MaintainResolution, Hint::Detail, CodecIntent::Detail),
        ScreenPreset::Text => (None, 15, MaintainResolution, Hint::Text, CodecIntent::Detail),
    };
    let (width, height) = (size.map(|(w, _)| w), size.map(|(_, h)| h));
    VideoTarget {
        capture: CaptureTarget { width, height, fps, ideal: false },
        max_width: width,
        max_height: height,
        max_framerate: Some(fps),
        bitrate: Some(SCREEN_BITRATE),
        degradation: Some(degradation),
        hint,
        codec_intent,
    }
}

/// A camera preset's settings. Balanced still asks for 640×480 at 30 fps explicitly, so
/// switching back from HD really reverts the camera, but leaves the sender to the browser.
pub fn camera_target(p: CameraPreset) -> VideoTarget {
    let (w, h, fps) = match p {
        CameraPreset::Smooth60 => (1280, 720, 60),
        CameraPreset::Balanced => (640, 480, 30),
        CameraPreset::Hd => (1280, 720, 30),
        CameraPreset::FullHd => (1920, 1080, 30),
        CameraPreset::DataSaver => (640, 360, 15),
    };
    let capture = CaptureTarget { width: Some(w), height: Some(h), fps, ideal: true };
    if p == CameraPreset::Balanced {
        return VideoTarget {
            capture,
            max_width: None,
            max_height: None,
            max_framerate: None,
            bitrate: None,
            degradation: None,
            hint: Hint::None,
            codec_intent: CodecIntent::Default,
        };
    }
    let (degradation, codec_intent) = match p {
        CameraPreset::Smooth60 => (Degradation::MaintainFramerate, CodecIntent::Motion),
        _ => (Degradation::Balanced, CodecIntent::Default),
    };
    VideoTarget {
        capture,
        max_width: Some(w),
        max_height: Some(h),
        max_framerate: Some(fps),
        bitrate: Some(CAMERA_BITRATE),
        degradation: Some(degradation),
        hint: Hint::None,
        codec_intent,
    }
}

/// How much to scale a frame down to fit the cap (`scaleResolutionDownBy`), never below 1.
/// It compares long side with long side and short with short, because a phone camera asked
/// for 1280×720 may deliver portrait 720×1280. Without both caps or a known frame size the
/// frame is left alone.
pub fn scale_down_by(frame_w: u32, frame_h: u32, max_w: Option<u32>, max_h: Option<u32>) -> f64 {
    let (Some(max_w), Some(max_h)) = (max_w, max_h) else {
        return 1.0;
    };
    if frame_w == 0 || frame_h == 0 || max_w == 0 || max_h == 0 {
        return 1.0;
    }
    let (long, short) = (frame_w.max(frame_h) as f64, frame_w.min(frame_h) as f64);
    let (cap_long, cap_short) = (max_w.max(max_h) as f64, max_w.min(max_h) as f64);
    (long / cap_long).max(short / cap_short).max(1.0)
}

/// `width × height × fps × bits_per_pixel`, rounded and clamped to the rule's floor and cap.
/// Unknown (zero) sizes get the floor.
pub fn max_bitrate_bps(width: u32, height: u32, fps: u32, rule: BitrateRule) -> u32 {
    let raw = width as f64 * height as f64 * fps as f64 * rule.bits_per_pixel;
    if !(raw.is_finite() && raw > 0.0) {
        return rule.floor_bps;
    }
    let cap = rule.cap_bps.max(rule.floor_bps);
    (raw.round().min(cap as f64) as u32).max(rule.floor_bps)
}

/// What to write into one sender's `RTCRtpSendParameters`. `None` means delete the key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SenderParams {
    pub max_bitrate_bps: Option<u32>,
    pub max_framerate: Option<u32>,
    pub scale_resolution_down_by: f64,
    pub degradation: Option<Degradation>,
}

/// The pure part of the RTP adapter, given the size the track really delivers
/// (`getSettings()`, 0 when unknown): the bitrate follows the size actually encoded, so a
/// smaller window share doesn't get a 1080p budget.
pub fn sender_params(t: &VideoTarget, frame_w: u32, frame_h: u32) -> SenderParams {
    let scale = scale_down_by(frame_w, frame_h, t.max_width, t.max_height);
    let (enc_w, enc_h) = if frame_w > 0 && frame_h > 0 {
        ((frame_w as f64 / scale).round() as u32, (frame_h as f64 / scale).round() as u32)
    } else {
        assumed_size(t)
    };
    let fps = t.max_framerate.unwrap_or(t.capture.fps);
    SenderParams {
        max_bitrate_bps: t.bitrate.map(|rule| max_bitrate_bps(enc_w, enc_h, fps, rule)),
        max_framerate: t.max_framerate,
        scale_resolution_down_by: scale,
        degradation: t.degradation,
    }
}

/// The size to budget for before the track reports one: the encode cap, else what the
/// capture asks for, else 1080p.
fn assumed_size(t: &VideoTarget) -> (u32, u32) {
    match (t.max_width, t.max_height, t.capture.width, t.capture.height) {
        (Some(w), Some(h), _, _) | (_, _, Some(w), Some(h)) => (w, h),
        _ => (1920, 1080),
    }
}

/// One `MediaCapabilities.encodingInfo({ type: "webrtc" })` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodecProbe {
    pub kind: CodecKind,
    pub supported: bool,
    pub smooth: bool,
    pub power_efficient: bool,
}

/// Codecs to try, best first. Motion wants a power-efficient (hardware) encoder, the one
/// with the least encode delay; VP8 closes it because every browser can send and receive
/// it. Detail takes AV1 or VP9 only when the browser says they keep up (`smooth`): their
/// screen-content tools are mostly software. Default may be empty, which means "keep the
/// codec the link negotiated". A kind with no probe counts as unsupported.
pub fn codec_ladder(intent: CodecIntent, probes: &[CodecProbe]) -> Vec<CodecKind> {
    let has = |kind: CodecKind, good: fn(&CodecProbe) -> bool| {
        probes.iter().any(|p| p.kind == kind && p.supported && good(p))
    };
    let efficient = |kind| has(kind, |p| p.power_efficient);
    let smooth = |kind| has(kind, |p| p.smooth);
    let motion = || {
        let mut ladder: Vec<CodecKind> =
            [CodecKind::H265, CodecKind::H264].into_iter().filter(|&k| efficient(k)).collect();
        ladder.push(CodecKind::Vp8);
        ladder
    };
    match intent {
        CodecIntent::Motion => motion(),
        CodecIntent::Detail => {
            let mut ladder: Vec<CodecKind> =
                [CodecKind::Av1, CodecKind::Vp9].into_iter().filter(|&k| smooth(k)).collect();
            for kind in motion() {
                if !ladder.contains(&kind) {
                    ladder.push(kind);
                }
            }
            ladder
        }
        CodecIntent::Default => [CodecKind::H264].into_iter().filter(|&k| efficient(k)).collect(),
    }
}

/// One entry of `RTCRtpSendParameters.codecs`, which lists what the receiver accepts in its
/// order of preference.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NegotiatedCodec {
    pub mime_type: String,
    pub sdp_fmtp_line: Option<String>,
}

/// Which negotiated entry to set as `encodings[0].codec`: the first ladder kind this link
/// negotiated, as the receiver listed it. H.264 counts only with `packetization-mode=1`
/// (mode 0 can't fragment frames bigger than one packet). VP9 prefers profile 0 (8-bit
/// 4:2:0, the one every decoder has). `None` when the link negotiated nothing on the
/// ladder: keep its default codec.
pub fn pick_negotiated(ladder: &[CodecKind], negotiated: &[NegotiatedCodec]) -> Option<usize> {
    ladder.iter().find_map(|&kind| {
        let mut of_kind =
            negotiated.iter().enumerate().filter(|(_, c)| CodecKind::from_mime(&c.mime_type) == Some(kind));
        let found = match kind {
            CodecKind::H264 => of_kind.find(|(_, c)| fmtp_param(c, "packetization-mode") == Some("1")),
            CodecKind::Vp9 => {
                let all: Vec<_> = of_kind.collect();
                let profile0 = all.iter().find(|(_, c)| matches!(fmtp_param(c, "profile-id"), None | Some("0")));
                profile0.or(all.first()).copied()
            }
            _ => of_kind.next(),
        };
        found.map(|(i, _)| i)
    })
}

/// A parameter of an `a=fmtp` line (`key=value;key=value`, spaces tolerated).
fn fmtp_param<'a>(codec: &'a NegotiatedCodec, key: &str) -> Option<&'a str> {
    codec
        .sdp_fmtp_line
        .as_deref()?
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
}

/// A `VideoEncoderConfig` for a future encode-once path (one WebCodecs encoder feeding every
/// viewer). Not used by the client today; it keeps that path on the same table.
///
/// There is deliberately no `scalability_mode`: Firefox rejects an explicit `"L1T1"`, so a
/// single layer must leave the field out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WebCodecsConfig {
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub bitrate: Option<u32>,
    /// Always `"realtime"`: Chrome's default, `"quality"`, buffers frames for rate control.
    pub latency_mode: &'static str,
    pub content_hint: &'static str,
}

/// The preset as a WebCodecs configuration for a source of the given size. WebCodecs needs
/// an explicit size, so the source is scaled down like a sender would and rounded down to
/// even numbers (4:2:0 chroma), at least 2.
pub fn webcodecs_config(t: &VideoTarget, codec: CodecKind, source_w: u32, source_h: u32) -> WebCodecsConfig {
    let (width, height) = if source_w > 0 && source_h > 0 {
        let scale = scale_down_by(source_w, source_h, t.max_width, t.max_height);
        (even_down(source_w as f64 / scale), even_down(source_h as f64 / scale))
    } else {
        let (w, h) = assumed_size(t);
        (even_down(w as f64), even_down(h as f64))
    };
    let framerate = t.max_framerate.unwrap_or(t.capture.fps);
    WebCodecsConfig {
        codec: webcodecs_codec(codec),
        width,
        height,
        framerate,
        bitrate: t.bitrate.map(|rule| max_bitrate_bps(width, height, framerate, rule)),
        latency_mode: "realtime",
        content_hint: t.hint.as_str(),
    }
}

/// Codec strings with levels high enough for 1080p60 and beyond.
fn webcodecs_codec(kind: CodecKind) -> &'static str {
    match kind {
        // Constrained baseline, level 5.2: what hardware encoders and every decoder support.
        CodecKind::H264 => "avc1.42e034",
        CodecKind::Vp8 => "vp8",
        // Profile 0, level 5.1, 8-bit.
        CodecKind::Vp9 => "vp09.00.51.08",
        // Main profile, level 5.1, main tier, 8-bit.
        CodecKind::Av1 => "av01.0.13M.08",
        // Main profile, level 5.1.
        CodecKind::H265 => "hvc1.1.6.L153.B0",
    }
}

fn even_down(v: f64) -> u32 {
    // A hair of slack so that 1920 / 1.5 never lands on 1279.999… and loses two pixels.
    // `as` saturates, and turns NaN into 0.
    let whole = (v + 1e-6).floor() as u32;
    (whole & !1).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// preset, capture (width, height, fps, ideal), encode cap, max fps, degradation, hint, codec intent
    type Capture = (Option<u32>, Option<u32>, u32, bool);
    type Row<P> = (P, Capture, Option<(u32, u32)>, Option<u32>, Option<Degradation>, Hint, CodecIntent);

    fn expected<P: Copy>(row: &Row<P>, bitrate: Option<BitrateRule>) -> VideoTarget {
        let (_, (width, height, fps, ideal), max, max_framerate, degradation, hint, codec_intent) = *row;
        VideoTarget {
            capture: CaptureTarget { width, height, fps, ideal },
            max_width: max.map(|(w, _)| w),
            max_height: max.map(|(_, h)| h),
            max_framerate,
            bitrate,
            degradation,
            hint,
            codec_intent,
        }
    }

    #[test]
    fn test_screen_presets_match_the_table() {
        use CodecIntent as C;
        use Degradation::{MaintainFramerate as Fr, MaintainResolution as Res};
        use ScreenPreset::*;
        let (s720, s1080) = (Some((1280, 720)), Some((1920, 1080)));
        let rows: [Row<ScreenPreset>; 5] = [
            (Fastest, (Some(1280), Some(720), 60, false), s720, Some(60), Some(Fr), Hint::Motion, C::Motion),
            (Smooth, (Some(1920), Some(1080), 60, false), s1080, Some(60), Some(Fr), Hint::Motion, C::Motion),
            (Balanced, (Some(1920), Some(1080), 30, false), s1080, Some(30), Some(Res), Hint::None, C::Default),
            (Sharp, (None, None, 30, false), None, Some(30), Some(Res), Hint::Detail, C::Detail),
            (Text, (None, None, 15, false), None, Some(15), Some(Res), Hint::Text, C::Detail),
        ];
        assert_eq!(rows.map(|r| r.0), ScreenPreset::ALL, "every preset, in ladder order");
        let rule = BitrateRule { bits_per_pixel: 0.1, floor_bps: 300_000, cap_bps: 20_000_000 };
        for row in &rows {
            assert_eq!(screen_target(row.0), expected(row, Some(rule)), "{:?}", row.0);
        }
    }

    #[test]
    fn test_camera_presets_match_the_table() {
        use CameraPreset::*;
        use CodecIntent as C;
        use Degradation::{Balanced as Bal, MaintainFramerate as Fr};
        let rows: [Row<CameraPreset>; 5] = [
            (Smooth60, (Some(1280), Some(720), 60, true), Some((1280, 720)), Some(60), Some(Fr), Hint::None, C::Motion),
            (Balanced, (Some(640), Some(480), 30, true), None, None, None, Hint::None, C::Default),
            (Hd, (Some(1280), Some(720), 30, true), Some((1280, 720)), Some(30), Some(Bal), Hint::None, C::Default),
            (FullHd, (Some(1920), Some(1080), 30, true), Some((1920, 1080)), Some(30), Some(Bal), Hint::None, C::Default),
            (DataSaver, (Some(640), Some(360), 15, true), Some((640, 360)), Some(15), Some(Bal), Hint::None, C::Default),
        ];
        assert_eq!(rows.map(|r| r.0), CameraPreset::ALL, "every preset, in ladder order");
        let rule = BitrateRule { bits_per_pixel: 0.08, floor_bps: 300_000, cap_bps: 6_000_000 };
        for row in &rows {
            let bitrate = (row.0 != Balanced).then_some(rule);
            assert_eq!(camera_target(row.0), expected(row, bitrate), "{:?}", row.0);
        }
    }

    #[test]
    fn test_key_presets_keep_their_promises() {
        let camera = camera_target(CameraPreset::Balanced);
        assert_eq!((camera.max_framerate, camera.bitrate, camera.degradation), (None, None, None), "browser defaults");
        assert_eq!((camera.max_width, camera.max_height), (None, None));

        for p in [ScreenPreset::Fastest, ScreenPreset::Smooth] {
            let t = screen_target(p);
            assert_eq!(t.degradation, Some(Degradation::MaintainFramerate), "{p:?}");
            assert_eq!((t.hint, t.codec_intent), (Hint::Motion, CodecIntent::Motion), "{p:?}");
            assert_eq!(t.max_framerate, Some(60));
        }

        let balanced = screen_target(ScreenPreset::Balanced);
        assert_eq!(balanced.degradation, Some(Degradation::MaintainResolution));
        assert_eq!((balanced.max_width, balanced.max_height), (Some(1920), Some(1080)));
        assert_eq!(balanced.hint, Hint::None);
        assert_eq!(balanced.hint.as_str(), "");

        let defaults = VideoPresets::default();
        assert_eq!((defaults.camera, defaults.screen), (CameraPreset::Balanced, ScreenPreset::Balanced));
        for p in ScreenPreset::ALL {
            assert!(!screen_target(p).capture.ideal, "screen sizes are caps, never upscaled");
        }
        for p in CameraPreset::ALL {
            assert!(camera_target(p).capture.ideal);
        }
    }

    #[test]
    fn test_ids_round_trip() {
        let screen: Vec<_> = ScreenPreset::ALL.iter().map(|p| p.id()).collect();
        assert_eq!(screen, ["fastest", "smooth", "balanced", "sharp", "text"]);
        let camera: Vec<_> = CameraPreset::ALL.iter().map(|p| p.id()).collect();
        assert_eq!(camera, ["smooth60", "balanced", "hd", "fullhd", "datasaver"]);
        for p in ScreenPreset::ALL {
            assert_eq!(ScreenPreset::from_id(p.id()), Some(p));
        }
        for p in CameraPreset::ALL {
            assert_eq!(CameraPreset::from_id(p.id()), Some(p));
        }
        assert_eq!(ScreenPreset::from_id("Fastest"), None);
        assert_eq!(CameraPreset::from_id(""), None);
        assert_eq!(ScreenPreset::default(), ScreenPreset::Balanced);
        assert_eq!(CameraPreset::default(), CameraPreset::Balanced);

        let degradations = [Degradation::MaintainFramerate, Degradation::MaintainResolution, Degradation::Balanced];
        let strings = degradations.map(Degradation::as_str);
        assert_eq!(strings, ["maintain-framerate", "maintain-resolution", "balanced"]);
        let hints = [Hint::None, Hint::Motion, Hint::Detail, Hint::Text].map(Hint::as_str);
        assert_eq!(hints, ["", "motion", "detail", "text"]);
        for kind in CodecKind::ALL {
            assert_eq!(CodecKind::from_mime(kind.mime()), Some(kind));
            assert_eq!(CodecKind::from_mime(&kind.mime().to_lowercase()), Some(kind));
        }
        assert_eq!(CodecKind::from_mime("video/h264"), Some(CodecKind::H264));
        for repair in ["video/rtx", "video/red", "video/ulpfec", "video/flexfec-03", "audio/opus", ""] {
            assert_eq!(CodecKind::from_mime(repair), None, "{repair}");
        }
    }

    #[test]
    fn test_scale_down_by_ignores_orientation_and_never_upscales() {
        let cap = (Some(1280), Some(720));
        assert_eq!(scale_down_by(1920, 1080, cap.0, cap.1), 1.5, "landscape");
        assert_eq!(scale_down_by(1080, 1920, cap.0, cap.1), 1.5, "portrait, same ratio");
        assert_eq!(scale_down_by(720, 1280, cap.0, cap.1), 1.0, "a phone's portrait 720p fits a 720p cap");
        assert_eq!(scale_down_by(1280, 720, cap.0, cap.1), 1.0, "exact fit");
        assert_eq!(scale_down_by(640, 480, cap.0, cap.1), 1.0, "smaller than the cap");
        assert_eq!(scale_down_by(2560, 1600, Some(1920), Some(1080)), 1600.0 / 1080.0, "the tighter side wins");
        assert_eq!(scale_down_by(1000, 2000, cap.0, cap.1), 2000.0 / 1280.0, "long side wins");
        assert_eq!(scale_down_by(1920, 1080, None, None), 1.0, "no cap");
        assert_eq!(scale_down_by(1920, 1080, Some(1280), None), 1.0, "half a cap");
        assert_eq!(scale_down_by(0, 0, cap.0, cap.1), 1.0, "unknown frame");
        assert_eq!(scale_down_by(1920, 0, cap.0, cap.1), 1.0, "half-known frame");
        assert_eq!(scale_down_by(1920, 1080, Some(0), Some(0)), 1.0, "zero cap");
        for (w, h) in [(1, 1), (u32::MAX, u32::MAX), (u32::MAX, 1), (3, 7)] {
            let s = scale_down_by(w, h, cap.0, cap.1);
            assert!(s.is_finite() && s >= 1.0, "{w}x{h} -> {s}");
        }
    }

    #[test]
    fn test_max_bitrate_is_clamped() {
        let screen = SCREEN_BITRATE;
        assert_eq!(max_bitrate_bps(1920, 1080, 30, screen), 6_220_800, "Balanced at 1080p");
        assert_eq!(max_bitrate_bps(1280, 720, 60, screen), 5_529_600, "Fastest");
        assert_eq!(max_bitrate_bps(1920, 1080, 60, screen), 12_441_600, "Smooth");
        assert_eq!(max_bitrate_bps(7680, 4320, 60, screen), 20_000_000, "cap");
        assert_eq!(max_bitrate_bps(320, 180, 15, screen), 300_000, "floor");
        assert_eq!(max_bitrate_bps(0, 1080, 30, screen), 300_000, "unknown size");
        assert_eq!(max_bitrate_bps(1920, 1080, 0, screen), 300_000, "zero fps");
        assert_eq!(max_bitrate_bps(640, 360, 15, CAMERA_BITRATE), 300_000, "Data saver sits on the floor");
        assert_eq!(max_bitrate_bps(1280, 720, 60, CAMERA_BITRATE), 4_423_680, "Smooth 60");
        assert_eq!(max_bitrate_bps(u32::MAX, u32::MAX, u32::MAX, CAMERA_BITRATE), 6_000_000);
        let nan = BitrateRule { bits_per_pixel: f64::NAN, ..SCREEN_BITRATE };
        assert_eq!(max_bitrate_bps(1920, 1080, 30, nan), 300_000);
        let inverted = BitrateRule { bits_per_pixel: 0.1, floor_bps: 500_000, cap_bps: 100_000 };
        assert_eq!(max_bitrate_bps(1920, 1080, 30, inverted), 500_000, "never panics on a bad rule");
    }

    #[test]
    fn test_sender_params_follow_the_real_frame() {
        let fastest = screen_target(ScreenPreset::Fastest);
        assert_eq!(
            sender_params(&fastest, 1920, 1080),
            SenderParams {
                max_bitrate_bps: Some(5_529_600),
                max_framerate: Some(60),
                scale_resolution_down_by: 1.5,
                degradation: Some(Degradation::MaintainFramerate),
            }
        );
        let small_window = sender_params(&fastest, 800, 600);
        assert_eq!(small_window.scale_resolution_down_by, 1.0);
        assert_eq!(small_window.max_bitrate_bps, Some(2_880_000), "budget for what is really encoded");
        assert_eq!(sender_params(&fastest, 0, 0).max_bitrate_bps, Some(5_529_600), "unknown size: the cap");

        let sharp = screen_target(ScreenPreset::Sharp);
        let params = sender_params(&sharp, 2560, 1440);
        assert_eq!(params.scale_resolution_down_by, 1.0, "no cap");
        assert_eq!(params.max_bitrate_bps, Some(11_059_200), "1440p at 30 fps");
        assert_eq!(sender_params(&sharp, 0, 0).max_bitrate_bps, Some(6_220_800), "unknown size, no cap: 1080p");

        let hd = camera_target(CameraPreset::Hd);
        let portrait = sender_params(&hd, 720, 1280);
        assert_eq!(portrait.scale_resolution_down_by, 1.0);
        assert_eq!(portrait.max_bitrate_bps, Some(2_211_840));

        assert_eq!(
            sender_params(&camera_target(CameraPreset::Balanced), 640, 480),
            SenderParams { max_bitrate_bps: None, max_framerate: None, scale_resolution_down_by: 1.0, degradation: None }
        );
        assert_eq!(sender_params(&camera_target(CameraPreset::Balanced), 1920, 1080).scale_resolution_down_by, 1.0);
    }

    fn probe(kind: CodecKind, supported: bool, smooth: bool, power_efficient: bool) -> CodecProbe {
        CodecProbe { kind, supported, smooth, power_efficient }
    }

    #[test]
    fn test_codec_ladder_per_intent() {
        use CodecKind::*;
        let all: Vec<_> = CodecKind::ALL.iter().map(|&k| probe(k, true, true, true)).collect();
        assert_eq!(codec_ladder(CodecIntent::Motion, &all), [H265, H264, Vp8]);
        assert_eq!(codec_ladder(CodecIntent::Detail, &all), [Av1, Vp9, H265, H264, Vp8]);
        assert_eq!(codec_ladder(CodecIntent::Default, &all), [H264]);

        assert_eq!(codec_ladder(CodecIntent::Motion, &[]), [Vp8], "VP8 is always there");
        assert_eq!(codec_ladder(CodecIntent::Detail, &[]), [Vp8]);
        assert!(codec_ladder(CodecIntent::Default, &[]).is_empty(), "keep the negotiated default");

        // A typical Linux desktop: software H.264 (not power efficient), AV1 smooth, VP9 too slow.
        let partial = [
            probe(H264, true, true, false),
            probe(H265, false, false, false),
            probe(Av1, true, true, false),
            probe(Vp9, true, false, false),
            probe(Vp8, false, false, false),
        ];
        assert_eq!(codec_ladder(CodecIntent::Motion, &partial), [Vp8]);
        assert_eq!(codec_ladder(CodecIntent::Detail, &partial), [Av1, Vp8]);
        assert!(codec_ladder(CodecIntent::Default, &partial).is_empty());

        // A Mac: hardware H.264 and H.265, no AV1 encoder.
        let mac = [
            probe(H264, true, true, true),
            probe(H265, true, true, true),
            probe(Av1, false, false, false),
            probe(Vp9, true, true, false),
        ];
        assert_eq!(codec_ladder(CodecIntent::Motion, &mac), [H265, H264, Vp8]);
        assert_eq!(codec_ladder(CodecIntent::Detail, &mac), [Vp9, H265, H264, Vp8]);
        assert_eq!(codec_ladder(CodecIntent::Default, &mac), [H264]);

        let efficient_but_unsupported = [probe(H264, false, true, true)];
        assert_eq!(codec_ladder(CodecIntent::Default, &efficient_but_unsupported), []);
    }

    fn codec(mime: &str, fmtp: Option<&str>) -> NegotiatedCodec {
        NegotiatedCodec { mime_type: mime.into(), sdp_fmtp_line: fmtp.map(Into::into) }
    }

    fn chrome() -> Vec<NegotiatedCodec> {
        vec![
            codec("video/VP8", None),
            codec("video/rtx", Some("apt=96")),
            codec("video/H264", Some("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f")),
            codec("video/H264", Some("level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42001f")),
            codec("video/H264", Some("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f")),
            codec("video/AV1", Some("level-idx=5;profile=0;tier=0")),
            codec("video/VP9", Some("profile-id=0")),
            codec("video/VP9", Some("profile-id=2")),
            codec("video/red", None),
            codec("video/ulpfec", None),
        ]
    }

    fn firefox() -> Vec<NegotiatedCodec> {
        vec![
            codec("video/VP8", Some("max-fs=12288;max-fr=60")),
            codec("video/VP9", Some("max-fs=12288;max-fr=60")),
            codec("video/H264", Some("profile-level-id=42e01f;level-asymmetry-allowed=1;packetization-mode=1")),
            codec("video/H264", Some("profile-level-id=42e01f;level-asymmetry-allowed=1")),
            codec("video/AV1", None),
        ]
    }

    fn safari() -> Vec<NegotiatedCodec> {
        vec![
            codec("video/H264", Some("level-asymmetry-allowed=1; packetization-mode=1; profile-level-id=640c1f")),
            codec("video/H264", Some("level-asymmetry-allowed=1; packetization-mode=1; profile-level-id=42e01f")),
            codec("video/H265", Some("level-id=93;profile-id=1;tier-flag=0;tx-mode=SRST")),
            codec("video/VP8", None),
            codec("video/VP9", Some("profile-id=0")),
        ]
    }

    #[test]
    fn test_pick_negotiated_against_real_browser_lists() {
        use CodecKind::*;
        let motion = [H265, H264, Vp8];
        let detail = [Av1, Vp9, H265, H264, Vp8];

        let chrome = chrome();
        assert_eq!(pick_negotiated(&motion, &chrome), Some(2), "H.264 with packetization-mode=1, receiver's order");
        assert_eq!(pick_negotiated(&detail, &chrome), Some(5), "AV1");
        assert_eq!(pick_negotiated(&[Vp9], &chrome), Some(6), "VP9 profile 0");
        assert_eq!(pick_negotiated(&[Vp8], &chrome), Some(0));
        assert_eq!(pick_negotiated(&[], &chrome), None, "empty ladder keeps the default");

        let firefox = firefox();
        assert_eq!(pick_negotiated(&motion, &firefox), Some(2));
        assert_eq!(pick_negotiated(&detail, &firefox), Some(4));
        assert_eq!(pick_negotiated(&[Vp9, Vp8], &firefox), Some(1), "no profile-id means profile 0");

        let safari = safari();
        assert_eq!(pick_negotiated(&motion, &safari), Some(2), "H.265");
        assert_eq!(pick_negotiated(&[H264, Vp8], &safari), Some(0), "first H.264 the receiver lists, spaces in fmtp");
        assert_eq!(pick_negotiated(&detail, &safari), Some(4), "no AV1: VP9");
    }

    #[test]
    fn test_pick_negotiated_falls_back_per_link() {
        use CodecKind::*;
        let detail = [Av1, Vp9, H265, H264, Vp8];
        let no_av1: Vec<_> = chrome().into_iter().filter(|c| c.mime_type != "video/AV1").collect();
        assert_eq!(pick_negotiated(&detail, &no_av1), Some(5), "VP9 profile 0, now one place earlier");

        let vp9_profile2_first = [codec("video/VP9", Some("profile-id=2")), codec("video/VP9", Some(" profile-id = 0 "))];
        assert_eq!(pick_negotiated(&[Vp9], &vp9_profile2_first), Some(1), "profile 0 preferred");
        assert_eq!(pick_negotiated(&[Vp9], &vp9_profile2_first[..1]), Some(0), "any VP9 beats none");

        let h264_mode0 = [
            codec("video/H264", Some("packetization-mode=0;profile-level-id=42e01f")),
            codec("video/H264", None),
            codec("video/vp8", None),
        ];
        assert_eq!(pick_negotiated(&[H264, Vp8], &h264_mode0), Some(2), "H.264 without mode 1 is skipped");

        let repair_only = vec![
            codec("video/rtx", Some("apt=96")),
            codec("video/red", None),
            codec("video/ulpfec", None),
            codec("video/flexfec-03", Some("repair-window=10000000")),
        ];
        assert_eq!(pick_negotiated(&[H265, H264, Vp8], &repair_only), None, "nothing usable");
        assert_eq!(pick_negotiated(&[H265, H264, Vp8], &[]), None, "not negotiated yet");
    }

    #[test]
    fn test_webcodecs_config_reuses_the_table() {
        let fastest = webcodecs_config(&screen_target(ScreenPreset::Fastest), CodecKind::H264, 1920, 1080);
        assert_eq!(
            fastest,
            WebCodecsConfig {
                codec: "avc1.42e034",
                width: 1280,
                height: 720,
                framerate: 60,
                bitrate: Some(5_529_600),
                latency_mode: "realtime",
                content_hint: "motion",
            }
        );

        let odd = webcodecs_config(&screen_target(ScreenPreset::Sharp), CodecKind::Vp9, 1365, 767);
        assert_eq!((odd.width, odd.height), (1364, 766), "even, rounded down");
        assert_eq!((odd.codec, odd.framerate, odd.content_hint), ("vp09.00.51.08", 30, "detail"));

        let scaled = webcodecs_config(&screen_target(ScreenPreset::Fastest), CodecKind::Av1, 1366, 769);
        assert!(scaled.width.is_multiple_of(2) && scaled.height.is_multiple_of(2), "{scaled:?}");
        assert!(scaled.width <= 1280 && scaled.height <= 720, "{scaled:?}");
        assert_eq!(scaled.codec, "av01.0.13M.08");

        let tiny = webcodecs_config(&screen_target(ScreenPreset::Text), CodecKind::Vp8, 1, 3);
        assert_eq!((tiny.width, tiny.height, tiny.codec), (2, 2, "vp8"));

        let camera = webcodecs_config(&camera_target(CameraPreset::Balanced), CodecKind::H265, 0, 0);
        assert_eq!((camera.width, camera.height, camera.framerate), (640, 480, 30), "unknown size: the capture's");
        assert_eq!((camera.bitrate, camera.content_hint, camera.codec), (None, "", "hvc1.1.6.L153.B0"));
        assert_eq!(camera.latency_mode, "realtime");
    }
}
