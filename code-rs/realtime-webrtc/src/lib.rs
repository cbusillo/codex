use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU16;
use std::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum RealtimeWebrtcError {
    #[error("{0}")]
    Message(String),
    #[error("realtime WebRTC is not supported on this platform")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeWebrtcEvent {
    Connected,
    LocalAudioLevel(u16),
    Closed,
    Failed(String),
}

pub type Result<T> = std::result::Result<T, RealtimeWebrtcError>;

pub struct StartedRealtimeWebrtcSession {
    pub offer_sdp: String,
    pub handle: RealtimeWebrtcSessionHandle,
    pub events: mpsc::Receiver<RealtimeWebrtcEvent>,
}

pub struct RealtimeWebrtcSessionHandle {
    local_audio_peak: Arc<AtomicU16>,
}

impl fmt::Debug for RealtimeWebrtcSessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealtimeWebrtcSessionHandle")
            .finish_non_exhaustive()
    }
}

impl RealtimeWebrtcSessionHandle {
    pub fn apply_answer_sdp(&self, answer_sdp: String) -> Result<()> {
        let _ = answer_sdp;
        Err(RealtimeWebrtcError::UnsupportedPlatform)
    }

    pub fn close(&self) {
        // Handles are only constructed by native WebRTC startup; the default
        // build returns UnsupportedPlatform before creating one.
    }

    pub fn local_audio_peak(&self) -> Arc<AtomicU16> {
        self.local_audio_peak.clone()
    }
}

pub struct RealtimeWebrtcSession;

impl RealtimeWebrtcSession {
    pub fn start() -> Result<StartedRealtimeWebrtcSession> {
        Err(RealtimeWebrtcError::UnsupportedPlatform)
    }
}
