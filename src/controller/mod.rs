use anyhow::Result;
#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{broadcast, Mutex};

use crate::config::{DriverKind, PositioningOptions};
use crate::core::{Channel, Command};
use crate::driver::{CommandOutcome, CommandRouter, SelectedChannelRx};
use crate::positioning::motion::{
    plan_motion, BlindMovement, MotionPlan, MotionRequest, MotionTimings,
};
use crate::positioning::motion_tasks::MotionTasks;
use crate::positioning::state::{find_blind, BlindPosition, PositionCache, PositionDelta};

type PositionSink = Arc<dyn Fn(&[PositionDelta]) + Send + Sync>;

/// Driver-agnostic control of channel selection, button presses, and position events.
pub struct BlindController {
    router: CommandRouter,
    driver_kind: DriverKind,
    operation_lock: Mutex<()>,
    positions: Arc<PositionCache>,
    timings: MotionTimings,
    motion_tasks: MotionTasks,
    position_tx: broadcast::Sender<Arc<[PositionDelta]>>,
    /// Optional sync fan-out (e.g. HomeKit HAP EVENT) installed once at startup.
    position_sink: StdMutex<Option<PositionSink>>,
}

impl fmt::Debug for BlindController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlindController")
            .field("driver_kind", &self.driver_kind)
            .field("position_subscribers", &self.position_tx.receiver_count())
            .finish_non_exhaustive()
    }
}

impl BlindController {
    pub(crate) async fn with_driver(
        config: crate::config::DriverConfig,
        positioning: PositioningOptions,
    ) -> Result<Self> {
        let driver_kind = config.kind;
        let router = CommandRouter::new(config).await?;
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            router,
            driver_kind,
            operation_lock: Mutex::new(()),
            positions: Arc::new(PositionCache::new()),
            timings: positioning.into(),
            motion_tasks: MotionTasks::default(),
            position_tx,
            position_sink: StdMutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) async fn with_driver_and_positions_for_test(
        config: crate::config::DriverConfig,
        positioning: PositioningOptions,
        positions: HashMap<u64, u8>,
    ) -> Result<Self> {
        let driver_kind = config.kind;
        let router = CommandRouter::new(config).await?;
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            router,
            driver_kind,
            operation_lock: Mutex::new(()),
            positions: Arc::new(PositionCache::from_positions(positions)),
            timings: positioning.into(),
            motion_tasks: MotionTasks::default(),
            position_tx,
            position_sink: StdMutex::new(None),
        })
    }

    /// Wire position deltas to a sync side channel (e.g. HomeKit EVENT push). Call once at startup.
    pub fn set_position_sink(&self, sink: PositionSink) {
        match self.position_sink.lock() {
            Ok(mut guard) => *guard = Some(sink),
            Err(_) => tracing::warn!("position sink mutex poisoned; side-channel events disabled"),
        }
    }

    #[cfg(test)]
    pub(crate) async fn lock_operations_for_test(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.operation_lock.lock().await
    }

    pub fn driver_kind(&self) -> DriverKind {
        self.driver_kind
    }

    /// Return the latest known channel selector state.
    pub fn current_selection(&self) -> Channel {
        self.router.selected_channel()
    }

    /// Subscribe to channel selector changes. New subscribers can immediately read
    /// the latest selection from the returned receiver.
    pub fn subscribe_selection(&self) -> SelectedChannelRx {
        self.router.subscribe_selected_channel()
    }

    /// Subscribe to target/current position changes (tests, future API clients).
    #[allow(dead_code)] // exercised in tests; reserved for non-HomeKit observers
    pub fn subscribe_positions(&self) -> broadcast::Receiver<Arc<[PositionDelta]>> {
        self.position_tx.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn emit_position_deltas_for_test(&self, deltas: &[PositionDelta]) {
        self.emit_position_deltas(deltas);
    }

    fn emit_position_deltas(&self, deltas: &[PositionDelta]) {
        if deltas.is_empty() {
            return;
        }
        let shared: Arc<[PositionDelta]> = Arc::from(deltas);
        let _ = self.position_tx.send(shared.clone());
        if let Ok(guard) = self.position_sink.lock() {
            if let Some(sink) = guard.as_ref() {
                sink(shared.as_ref());
            }
        }
    }

    pub async fn position_snapshot(&self) -> Vec<BlindPosition> {
        self.positions.snapshot().await
    }

    #[cfg(test)]
    pub async fn position_for_aid(&self, aid: u64) -> BlindPosition {
        self.positions
            .snapshot()
            .await
            .into_iter()
            .find(|position| position.aid == aid)
            .unwrap_or_else(|| BlindPosition::default_for_aid(aid))
    }

    pub async fn set_target_positions(
        self: &Arc<Self>,
        targets: Vec<(u64, u8)>,
    ) -> Result<Vec<PositionDelta>> {
        let deltas = {
            let _guard = self.operation_lock.lock().await;
            let requests = self.build_motion_requests(targets).await;
            match plan_motion(&requests) {
                MotionPlan::NoOp => Vec::new(),
                MotionPlan::CancelAndSnap { requests } => {
                    self.cancel_inflight_and_snap(requests).await?
                }
                MotionPlan::Travel { starts, movements } => {
                    self.execute_travel(starts, movements).await?
                }
            }
        };
        self.emit_position_deltas(&deltas);
        Ok(deltas)
    }

    async fn build_motion_requests(&self, targets: Vec<(u64, u8)>) -> Vec<MotionRequest> {
        let mut requests = Vec::with_capacity(targets.len());
        for (aid, target) in targets {
            let Some(blind) = find_blind(aid) else {
                tracing::warn!(aid, "ignoring position target for unknown accessory");
                continue;
            };
            let target = target.min(100);
            let position = self.positions.position_for_aid(aid).await;
            if position.target == target {
                continue;
            }
            requests.push(MotionRequest {
                blind,
                current: position.current,
                target,
                timing: self.timings.for_channel(blind.channel),
            });
        }
        requests
    }

    async fn cancel_inflight_and_snap(
        &self,
        requests: Vec<MotionRequest>,
    ) -> Result<Vec<PositionDelta>> {
        let mut deltas = Vec::new();
        for request in requests {
            if self.motion_tasks.cancel(request.blind.aid).await {
                self.router
                    .execute_on(request.blind.channel, Command::Stop)
                    .await?;
            }
            deltas.extend(
                self.positions
                    .apply_blind_current(request.blind, request.target)
                    .await,
            );
        }
        Ok(deltas)
    }

    async fn execute_travel(
        self: &Arc<Self>,
        starts: Vec<crate::positioning::motion::DriverStart>,
        movements: Vec<BlindMovement>,
    ) -> Result<Vec<PositionDelta>> {
        for movement in &movements {
            self.motion_tasks.cancel(movement.blind.aid).await;
        }

        for start in &starts {
            self.router.execute_on(start.channel, start.command).await?;
        }

        let mut deltas = Vec::new();
        for movement in movements {
            deltas.extend(
                self.positions
                    .apply_target(movement.blind, movement.target, movement.status)
                    .await,
            );
            self.schedule_completion(movement).await;
        }
        Ok(deltas)
    }

    /// Run a client command as one logical operation. `Select` changes the
    /// selected channel; action commands with an explicit channel target that
    /// channel directly without changing logical selection when the driver
    /// supports that distinction.
    pub async fn execute(
        &self,
        command: Command,
        channel: Option<Channel>,
    ) -> Result<CommandOutcome> {
        let (outcome, deltas) = {
            let _guard = self.operation_lock.lock().await;
            if command == Command::Select {
                self.router.execute(command, channel).await?;
                let target = self.current_selection();
                self.complete_command(target, command).await
            } else if let Some(channel) = channel {
                self.router.execute_on(channel, command).await?;
                self.complete_command(channel, command).await
            } else {
                self.router.execute(command, None).await?;
                let target = self.current_selection();
                self.complete_command(target, command).await
            }
        };
        self.emit_position_deltas(&deltas);
        Ok(outcome)
    }

    /// Run an action command directly on `channel`. RTS can do this without
    /// changing public selection state; Telis may update selection because
    /// targeting a channel requires moving the physical selector.
    #[cfg(test)]
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        if command == Command::Select {
            anyhow::bail!("select is not a direct targeted command");
        }
        let (outcome, deltas) = {
            let _guard = self.operation_lock.lock().await;
            self.router.execute_on(channel, command).await?;
            self.complete_command(channel, command).await
        };
        self.emit_position_deltas(&deltas);
        Ok(outcome)
    }

    #[cfg(test)]
    pub(crate) fn operations(&self) -> Vec<crate::driver::ProtocolOperation> {
        self.router.operations()
    }

    async fn complete_command(
        &self,
        channel: Channel,
        command: Command,
    ) -> (CommandOutcome, Vec<PositionDelta>) {
        let inferred_position = infer_position(command);
        let deltas = if let Some(position) = inferred_position {
            self.motion_tasks.cancel_channel(channel).await;
            self.positions.apply_for_channel(channel, position).await
        } else {
            Vec::new()
        };
        (CommandOutcome { inferred_position }, deltas)
    }

    async fn schedule_completion(self: &Arc<Self>, movement: BlindMovement) {
        let controller = self.clone();
        let generation = self.motion_tasks.replace(movement.blind.aid, None).await;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(movement.duration).await;
            let deltas = {
                let _guard = controller.operation_lock.lock().await;
                if !controller
                    .motion_tasks
                    .is_current(movement.blind.aid, generation)
                    .await
                {
                    return;
                }
                if movement.stop_at_end {
                    if let Err(e) = controller
                        .router
                        .execute_on(movement.blind.channel, Command::Stop)
                        .await
                    {
                        tracing::warn!(
                            aid = movement.blind.aid,
                            channel = %movement.blind.channel,
                            "failed to stop timed motion: {e}"
                        );
                        return;
                    }
                }
                if !controller
                    .motion_tasks
                    .is_current(movement.blind.aid, generation)
                    .await
                {
                    return;
                }
                let deltas = controller
                    .positions
                    .apply_blind_current(movement.blind, movement.target)
                    .await;
                controller
                    .motion_tasks
                    .remove_if_current(movement.blind.aid, generation)
                    .await;
                deltas
            };
            controller.emit_position_deltas(&deltas);
        });
        self.motion_tasks
            .attach_handle(movement.blind.aid, generation, handle)
            .await;
    }
}

fn infer_position(command: Command) -> Option<u8> {
    match command {
        Command::Up => Some(100),
        Command::Down => Some(0),
        Command::Stop | Command::Select | Command::Prog | Command::ProgLong => None,
    }
}

#[cfg(test)]
mod tests;
