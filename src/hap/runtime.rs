use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

use crate::hap::state::HapState;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Aid(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Iid(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CharacteristicId {
    pub aid: Aid,
    pub iid: Iid,
}

impl CharacteristicId {
    pub fn new(aid: u64, iid: u64) -> Self {
        Self {
            aid: Aid(aid),
            iid: Iid(iid),
        }
    }
}

impl Hash for CharacteristicId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.aid.0.hash(state);
        self.iid.0.hash(state);
    }
}

#[derive(Clone, Debug)]
pub struct CharacteristicWrite {
    pub id: CharacteristicId,
    pub value: Option<Value>,
    pub ev: Option<bool>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HapStatus {
    Success = 0,
    ReadOnly = -70404,
    WriteOnly = -70405,
    NotificationNotSupported = -70406,
    ResourceDoesNotExist = -70409,
    InvalidValue = -70410,
}

impl HapStatus {
    pub fn code(self) -> i64 {
        self as i64
    }
}

#[derive(Clone, Debug)]
pub struct CharacteristicRead {
    pub id: CharacteristicId,
    pub value: Option<Value>,
    pub status: HapStatus,
}

impl CharacteristicRead {
    pub fn success(id: CharacteristicId, value: Value) -> Self {
        Self {
            id,
            value: Some(value),
            status: HapStatus::Success,
        }
    }

    pub fn error(id: CharacteristicId, status: HapStatus) -> Self {
        Self {
            id,
            value: None,
            status,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CharacteristicWriteStatus {
    pub id: CharacteristicId,
    pub status: HapStatus,
}

impl CharacteristicWriteStatus {
    pub fn success(id: CharacteristicId) -> Self {
        Self {
            id,
            status: HapStatus::Success,
        }
    }

    pub fn error(id: CharacteristicId, status: HapStatus) -> Self {
        Self { id, status }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CharacteristicWriteOutcome {
    pub statuses: Vec<CharacteristicWriteStatus>,
    pub events: Vec<CharacteristicEvent>,
}

impl CharacteristicWriteOutcome {
    pub fn all_success(&self) -> bool {
        self.statuses
            .iter()
            .all(|status| status.status == HapStatus::Success)
    }
}

#[derive(Clone, Debug)]
pub struct CharacteristicEvent {
    pub id: CharacteristicId,
    pub value: Value,
}

pub type Subscriptions = HashSet<CharacteristicId>;
pub type HapFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait HapStore: Send + Sync + 'static {
    fn load_state(&self) -> Result<Option<HapState>>;
    fn save_state(&self, state: &HapState) -> Result<()>;
}

pub trait HapAccessoryApp: Send + Sync + 'static {
    fn accessories(&self) -> HapFuture<'_, Value>;

    fn read_characteristics<'a>(
        &'a self,
        ids: &'a [CharacteristicId],
    ) -> HapFuture<'a, Vec<CharacteristicRead>>;

    fn write_characteristics<'a>(
        &'a self,
        writes: Vec<CharacteristicWrite>,
        subscriptions: &'a mut Subscriptions,
    ) -> HapFuture<'a, CharacteristicWriteOutcome>;
}

pub struct HapRuntime<A, S>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    pub state: Mutex<HapState>,
    pub store: S,
    pub app: Arc<A>,
    events: broadcast::Sender<Vec<CharacteristicEvent>>,
}

impl<A, S> HapRuntime<A, S>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    pub fn new(
        state: HapState,
        store: S,
        app: Arc<A>,
        events: broadcast::Sender<Vec<CharacteristicEvent>>,
    ) -> Self {
        Self {
            state: Mutex::new(state),
            store,
            app,
            events,
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Vec<CharacteristicEvent>> {
        self.events.subscribe()
    }

    pub fn event_sender(&self) -> broadcast::Sender<Vec<CharacteristicEvent>> {
        self.events.clone()
    }

    pub fn publish_events(&self, events: Vec<CharacteristicEvent>) {
        if !events.is_empty() {
            let _ = self.events.send(events);
        }
    }
}
