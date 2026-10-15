use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum VoiceInputPhase {
    #[default]
    Idle,
    Starting,
    Active {
        call_id: Uuid,
        thread_id: String,
        transcript_channel_id: u64,
    },
    Ending,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputRoute {
    Ignore,
    Ordinary,
    Voice { call_id: Uuid, thread_id: String },
}

impl VoiceInputPhase {
    pub(crate) fn allow_ordinary(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub(crate) fn route(
        &self,
        channel_id: u64,
        home_channel_id: u64,
        mentioned: bool,
    ) -> InputRoute {
        if let Self::Active {
            call_id,
            thread_id,
            transcript_channel_id,
        } = self
        {
            if channel_id == *transcript_channel_id {
                return InputRoute::Voice {
                    call_id: *call_id,
                    thread_id: thread_id.clone(),
                };
            }
        }

        if channel_id == home_channel_id || mentioned {
            InputRoute::Ordinary
        } else {
            InputRoute::Ignore
        }
    }
}

pub(crate) struct VoiceInputGate {
    pub state: Mutex<VoiceInputPhase>,
    pub ready: Notify,
}

impl Default for VoiceInputGate {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceInputGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(VoiceInputPhase::Idle),
            ready: Notify::new(),
        }
    }

    pub(crate) async fn set(&self, phase: VoiceInputPhase) {
        let became_idle = phase == VoiceInputPhase::Idle;
        *self.state.lock().await = phase;
        if became_idle {
            self.ready.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn routes_voice_thread_before_home_or_mention_rules() {
        let call_id = Uuid::new_v4();
        let active = VoiceInputPhase::Active {
            call_id,
            thread_id: "thread-current".to_owned(),
            transcript_channel_id: 30,
        };

        assert_eq!(
            active.route(30, 10, false),
            InputRoute::Voice {
                call_id,
                thread_id: "thread-current".to_owned(),
            }
        );
        assert_eq!(active.route(10, 10, false), InputRoute::Ordinary);
        assert_eq!(active.route(20, 10, true), InputRoute::Ordinary);
        assert_eq!(active.route(20, 10, false), InputRoute::Ignore);
    }

    #[test]
    fn only_idle_allows_immediate_ordinary_submission() {
        assert!(VoiceInputPhase::Idle.allow_ordinary());
        for phase in [
            VoiceInputPhase::Starting,
            VoiceInputPhase::Active {
                call_id: Uuid::new_v4(),
                thread_id: "thread".to_owned(),
                transcript_channel_id: 30,
            },
            VoiceInputPhase::Ending,
            VoiceInputPhase::Blocked,
        ] {
            assert!(!phase.allow_ordinary());
        }
    }

    #[tokio::test]
    async fn setting_phase_waits_for_admission_guard_and_idle_signals_ready() {
        let gate = std::sync::Arc::new(VoiceInputGate::new());
        let guard = gate.state.lock().await;
        let setter_gate = gate.clone();
        let setter = tokio::spawn(async move {
            setter_gate.set(VoiceInputPhase::Starting).await;
        });
        tokio::task::yield_now().await;
        assert_eq!(*guard, VoiceInputPhase::Idle);
        drop(guard);
        setter.await.unwrap();
        assert_eq!(*gate.state.lock().await, VoiceInputPhase::Starting);

        gate.set(VoiceInputPhase::Idle).await;
        tokio::time::timeout(Duration::from_millis(50), gate.ready.notified())
            .await
            .expect("idle transition should wake the queue drainer");
    }
}
