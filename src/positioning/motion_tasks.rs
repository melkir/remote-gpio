//! In-flight timed motion task handles (generation tokens + abort).

use std::collections::HashMap;

use tokio::sync::Mutex;

use crate::core::Channel;
use crate::positioning::state::BLINDS;

#[derive(Debug, Default)]
pub(crate) struct MotionTasks {
    tasks: Mutex<HashMap<u64, MotionTaskState>>,
}

#[derive(Debug, Default)]
struct MotionTaskState {
    generation: u64,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl MotionTasks {
    pub async fn replace(&self, aid: u64, handle: Option<tokio::task::JoinHandle<()>>) -> u64 {
        let mut tasks = self.tasks.lock().await;
        let state = tasks.entry(aid).or_default();
        state.generation = state.generation.wrapping_add(1);
        if let Some(old) = state.handle.take() {
            old.abort();
        }
        state.handle = handle;
        state.generation
    }

    pub async fn attach_handle(
        &self,
        aid: u64,
        generation: u64,
        handle: tokio::task::JoinHandle<()>,
    ) {
        let mut tasks = self.tasks.lock().await;
        let state = tasks.entry(aid).or_default();
        if state.generation == generation {
            state.handle = Some(handle);
        } else {
            handle.abort();
        }
    }

    pub async fn cancel(&self, aid: u64) -> bool {
        let mut tasks = self.tasks.lock().await;
        Self::cancel_state(tasks.get_mut(&aid))
    }

    pub async fn cancel_channel(&self, channel: Channel) {
        let aids: Vec<u64> = match channel {
            Channel::All => BLINDS.iter().map(|blind| blind.aid).collect(),
            _ => BLINDS
                .iter()
                .filter(|blind| blind.channel == channel)
                .map(|blind| blind.aid)
                .collect(),
        };
        let mut tasks = self.tasks.lock().await;
        for aid in aids {
            Self::cancel_state(tasks.get_mut(&aid));
        }
    }

    fn cancel_state(state: Option<&mut MotionTaskState>) -> bool {
        let Some(state) = state else {
            return false;
        };
        state.generation = state.generation.wrapping_add(1);
        let Some(old) = state.handle.take() else {
            return false;
        };
        old.abort();
        true
    }

    pub async fn is_current(&self, aid: u64, generation: u64) -> bool {
        self.tasks
            .lock()
            .await
            .get(&aid)
            .is_some_and(|state| state.generation == generation)
    }

    pub async fn remove_if_current(&self, aid: u64, generation: u64) {
        let mut tasks = self.tasks.lock().await;
        if let Some(state) = tasks.get_mut(&aid) {
            if state.generation == generation {
                state.handle = None;
            }
        }
    }
}
