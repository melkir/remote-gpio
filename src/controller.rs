use anyhow::Result;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{broadcast, Mutex};

use crate::config::{DriverKind, PositioningOptions};
use crate::core::{Channel, Command};
use crate::driver::{CommandOutcome, CommandRouter, SelectedChannelRx};
use crate::positioning::motion::{plan_motion, BlindMovement, MotionRequest, MotionTimings};
use crate::positioning::state::{find_blind, BlindPosition, PositionCache, PositionDelta, BLINDS};

type PositionHook = Arc<dyn Fn(Vec<PositionDelta>) + Send + Sync>;

/// Driver-agnostic control of channel selection, button presses, and position events.
pub struct BlindController {
    router: CommandRouter,
    driver_kind: DriverKind,
    operation_lock: Mutex<()>,
    positions: Arc<PositionCache>,
    timings: MotionTimings,
    motion_tasks: MotionTasks,
    /// Fan-out of position target/current changes for tests and future API clients.
    position_tx: broadcast::Sender<Vec<PositionDelta>>,
    /// Installed by `homekit::start` to push HAP EVENT frames without a background task.
    position_hook: StdMutex<Option<PositionHook>>,
}

impl fmt::Debug for BlindController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hook_attached = self
            .position_hook
            .lock()
            .map(|hook| hook.is_some())
            .unwrap_or(false);
        f.debug_struct("BlindController")
            .field("driver_kind", &self.driver_kind)
            .field("position_hook_attached", &hook_attached)
            .finish_non_exhaustive()
    }
}

impl BlindController {
    pub async fn with_driver(
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
            position_hook: StdMutex::new(None),
        })
    }

    /// Wire position deltas to a side channel (e.g. HomeKit EVENT push). Call once at startup.
    pub fn attach_position_hook(&self, hook: PositionHook) {
        match self.position_hook.lock() {
            Ok(mut guard) => *guard = Some(hook),
            Err(_) => tracing::warn!("position hook mutex poisoned; HomeKit events disabled"),
        }
    }

    #[cfg(test)]
    pub(crate) async fn with_driver_and_positions_for_test(
        config: crate::config::DriverConfig,
        positioning: PositioningOptions,
        positions: std::collections::HashMap<u64, u8>,
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
            position_hook: StdMutex::new(None),
        })
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

    /// Subscribe to target/current position changes (tests; future API clients).
    #[cfg(test)]
    pub fn subscribe_positions(&self) -> broadcast::Receiver<Vec<PositionDelta>> {
        self.position_tx.subscribe()
    }

    fn emit_position_deltas(&self, deltas: Vec<PositionDelta>) {
        if deltas.is_empty() {
            return;
        }
        if let Ok(guard) = self.position_hook.lock() {
            if let Some(hook) = guard.as_ref() {
                hook(deltas.clone());
            }
        }
        let _ = self.position_tx.send(deltas);
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
            .unwrap_or(BlindPosition {
                aid,
                current: 100,
                target: 100,
                status: crate::positioning::state::STATUS_STOPPED,
            })
    }

    pub async fn set_target_positions(
        self: &Arc<Self>,
        targets: Vec<(u64, u8)>,
    ) -> Result<Vec<PositionDelta>> {
        let _guard = self.operation_lock.lock().await;
        let mut requests = Vec::with_capacity(targets.len());
        for (aid, target) in targets {
            let Some(blind) = find_blind(aid) else {
                continue;
            };
            let target = target.min(100);
            if self.positions.get_target(aid).await == target {
                continue;
            }
            requests.push(MotionRequest {
                blind,
                current: self.positions.get_current(aid).await,
                target,
                timing: self.timings.for_channel(blind.channel),
            });
        }

        let plan = plan_motion(&requests);
        if plan.movements.is_empty() {
            let mut deltas = Vec::new();
            for request in requests {
                if self.motion_tasks.cancel(request.blind.aid).await {
                    self.router
                        .execute_on(request.blind.channel, Command::Stop)
                        .await?;
                    deltas.extend(
                        self.positions
                            .apply_blind_current(request.blind, request.target)
                            .await,
                    );
                }
            }
            self.emit_position_deltas(deltas.clone());
            return Ok(deltas);
        }

        for movement in &plan.movements {
            self.motion_tasks.cancel(movement.blind.aid).await;
        }

        for start in &plan.starts {
            self.router.execute_on(start.channel, start.command).await?;
        }

        let mut deltas = Vec::new();
        for movement in plan.movements {
            deltas.extend(
                self.positions
                    .apply_target(movement.blind, movement.target, movement.status)
                    .await,
            );
            self.schedule_completion(movement).await;
        }
        self.emit_position_deltas(deltas.clone());
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
        let _guard = self.operation_lock.lock().await;
        if command == Command::Select {
            self.router.execute(command, channel).await?;
            let target = self.current_selection();
            return Ok(self.complete_command(target, command).await);
        }

        if let Some(channel) = channel {
            self.router.execute_on(channel, command).await?;
            return Ok(self.complete_command(channel, command).await);
        }

        self.router.execute(command, None).await?;
        let target = self.current_selection();
        Ok(self.complete_command(target, command).await)
    }

    /// Run an action command directly on `channel`. RTS can do this without
    /// changing public selection state; Telis may update selection because
    /// targeting a channel requires moving the physical selector.
    #[cfg(test)]
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        self.execute_on_inner(channel, command).await
    }

    #[cfg(test)]
    async fn execute_on_inner(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        if command == Command::Select {
            anyhow::bail!("select is not a direct targeted command");
        }
        let _guard = self.operation_lock.lock().await;
        self.router.execute_on(channel, command).await?;
        Ok(self.complete_command(channel, command).await)
    }

    #[cfg(test)]
    pub(crate) fn operations(&self) -> Vec<crate::driver::ProtocolOperation> {
        self.router.operations()
    }

    async fn complete_command(&self, channel: Channel, command: Command) -> CommandOutcome {
        let inferred_position = infer_position(command);
        if let Some(position) = inferred_position {
            self.motion_tasks.cancel_channel(channel).await;
            let deltas = self.positions.apply_for_channel(channel, position).await;
            self.emit_position_deltas(deltas);
        }
        CommandOutcome { inferred_position }
    }

    async fn schedule_completion(self: &Arc<Self>, movement: BlindMovement) {
        let controller = self.clone();
        let generation = self.motion_tasks.replace(movement.blind.aid, None).await;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(movement.duration).await;
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
            controller.emit_position_deltas(deltas);
            controller
                .motion_tasks
                .remove_if_current(movement.blind.aid, generation)
                .await;
        });
        self.motion_tasks
            .attach_handle(movement.blind.aid, generation, handle)
            .await;
    }
}

#[derive(Debug, Default)]
struct MotionTasks {
    tasks: Mutex<HashMap<u64, MotionTaskState>>,
}

#[derive(Debug, Default)]
struct MotionTaskState {
    generation: u64,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl MotionTasks {
    async fn replace(&self, aid: u64, handle: Option<tokio::task::JoinHandle<()>>) -> u64 {
        let mut tasks = self.tasks.lock().await;
        let state = tasks.entry(aid).or_default();
        state.generation = state.generation.wrapping_add(1);
        if let Some(old) = state.handle.take() {
            old.abort();
        }
        state.handle = handle;
        state.generation
    }

    async fn attach_handle(&self, aid: u64, generation: u64, handle: tokio::task::JoinHandle<()>) {
        let mut tasks = self.tasks.lock().await;
        let state = tasks.entry(aid).or_default();
        if state.generation == generation {
            state.handle = Some(handle);
        } else {
            handle.abort();
        }
    }

    async fn cancel(&self, aid: u64) -> bool {
        let mut tasks = self.tasks.lock().await;
        let Some(state) = tasks.get_mut(&aid) else {
            return false;
        };
        state.generation = state.generation.wrapping_add(1);
        let Some(old) = state.handle.take() else {
            return false;
        };
        old.abort();
        true
    }

    async fn cancel_channel(&self, channel: Channel) {
        let aids = match channel {
            Channel::All => BLINDS.iter().map(|blind| blind.aid).collect::<Vec<_>>(),
            _ => BLINDS
                .iter()
                .filter(|blind| blind.channel == channel)
                .map(|blind| blind.aid)
                .collect(),
        };
        let mut tasks = self.tasks.lock().await;
        for aid in aids {
            if let Some(state) = tasks.get_mut(&aid) {
                state.generation = state.generation.wrapping_add(1);
                if let Some(handle) = state.handle.take() {
                    handle.abort();
                }
            }
        }
    }

    async fn is_current(&self, aid: u64, generation: u64) -> bool {
        self.tasks
            .lock()
            .await
            .get(&aid)
            .is_some_and(|state| state.generation == generation)
    }

    async fn remove_if_current(&self, aid: u64, generation: u64) {
        let mut tasks = self.tasks.lock().await;
        if let Some(state) = tasks.get_mut(&aid) {
            if state.generation == generation {
                state.handle = None;
            }
        }
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::time::{timeout, Duration};

    fn controller_config() -> crate::config::PositioningOptions {
        crate::config::PositioningOptions::default()
    }

    #[test]
    fn position_inference_only_tracks_directional_extremes() {
        assert_eq!(infer_position(Command::Up), Some(100));
        assert_eq!(infer_position(Command::Down), Some(0));
        assert_eq!(infer_position(Command::Stop), None);
        assert_eq!(infer_position(Command::Select), None);
        assert_eq!(infer_position(Command::Prog), None);
    }

    #[tokio::test]
    async fn client_command_with_channel_targets_without_selection() {
        let controller =
            BlindController::with_driver(crate::config::DriverConfig::fake(), controller_config())
                .await
                .unwrap();

        controller
            .execute(Command::Up, Some(Channel::L3))
            .await
            .unwrap();

        assert_eq!(controller.current_selection(), Channel::L1);
        assert_eq!(controller.driver_kind(), DriverKind::Fake);
        assert_eq!(
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::L3,
                command: Command::Up,
            }]
        );
    }

    #[tokio::test]
    async fn controller_operations_wait_behind_operation_lock() {
        let controller = Arc::new(
            BlindController::with_driver(crate::config::DriverConfig::fake(), controller_config())
                .await
                .unwrap(),
        );
        let guard = controller.operation_lock.lock().await;
        let pending_controller = controller.clone();

        let operation = tokio::spawn(async move {
            pending_controller
                .execute(Command::Up, Some(Channel::L2))
                .await
        });

        assert!(timeout(Duration::from_millis(10), async {
            while !operation.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err());

        drop(guard);
        operation.await.unwrap().unwrap();
        assert_eq!(
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::L2,
                command: Command::Up,
            }]
        );
    }

    #[tokio::test]
    async fn target_position_writes_wait_behind_operation_lock() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                crate::config::PositioningOptions {
                    l1: crate::config::BlindTimingOptions {
                        open_ms: 50,
                        close_ms: 50,
                    },
                    ..crate::config::PositioningOptions::default()
                },
                HashMap::from([(2, 100)]),
            )
            .await
            .unwrap(),
        );
        let guard = controller.operation_lock.lock().await;
        let pending_controller = controller.clone();

        let operation =
            tokio::spawn(
                async move { pending_controller.set_target_positions(vec![(2, 50)]).await },
            );

        assert!(timeout(Duration::from_millis(10), async {
            while !operation.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err());

        drop(guard);
        operation.await.unwrap().unwrap();
        assert_eq!(
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::L1,
                command: Command::Down,
            }]
        );
    }

    #[tokio::test]
    async fn execute_on_rejects_select() {
        let controller =
            BlindController::with_driver(crate::config::DriverConfig::fake(), controller_config())
                .await
                .unwrap();

        let err = controller
            .execute_on(Channel::L2, Command::Select)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("select is not a direct targeted command"));
        assert_eq!(controller.operations(), Vec::new());
    }

    #[tokio::test]
    async fn target_position_updates_shared_position_model() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                crate::config::PositioningOptions {
                    l1: crate::config::BlindTimingOptions {
                        open_ms: 2,
                        close_ms: 2,
                    },
                    ..crate::config::PositioningOptions::default()
                },
                HashMap::from([(2, 100)]),
            )
            .await
            .unwrap(),
        );

        let deltas = controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        assert_eq!(deltas[0].target, Some(50));
        assert_eq!(controller.position_for_aid(2).await.target, 50);
        tokio::time::sleep(Duration::from_millis(3)).await;
        assert_eq!(controller.position_for_aid(2).await.current, 50);
    }

    #[tokio::test]
    async fn target_position_matching_cached_current_is_noop() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                controller_config(),
                HashMap::from([(2, 50)]),
            )
            .await
            .unwrap(),
        );
        let rx = controller.subscribe_positions();

        let deltas = controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        assert!(deltas.is_empty());
        assert!(rx.is_empty());
        assert_eq!(controller.operations(), Vec::new());
        assert_eq!(controller.position_for_aid(2).await.current, 50);
        assert_eq!(controller.position_for_aid(2).await.target, 50);
    }

    #[tokio::test]
    async fn target_position_matching_pending_target_is_noop() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                crate::config::PositioningOptions {
                    l1: crate::config::BlindTimingOptions {
                        open_ms: 50,
                        close_ms: 50,
                    },
                    ..crate::config::PositioningOptions::default()
                },
                HashMap::from([(2, 100)]),
            )
            .await
            .unwrap(),
        );
        controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();
        let rx = controller.subscribe_positions();

        let deltas = controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        assert!(deltas.is_empty());
        assert!(rx.is_empty());
        assert_eq!(
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::L1,
                command: Command::Down,
            }]
        );
        assert_eq!(controller.position_for_aid(2).await.current, 100);
        assert_eq!(controller.position_for_aid(2).await.target, 50);
    }

    #[tokio::test]
    async fn position_hook_runs_once_per_non_empty_emit() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                controller_config(),
                HashMap::from([(2, 100)]),
            )
            .await
            .unwrap(),
        );
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let hook_calls_for_hook = hook_calls.clone();
        let captured_for_hook = captured.clone();
        controller.attach_position_hook(Arc::new(move |deltas| {
            hook_calls_for_hook.fetch_add(1, Ordering::SeqCst);
            *captured_for_hook.lock().unwrap() = deltas;
        }));
        let mut rx = controller.subscribe_positions();

        controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
        let hooked = captured.lock().unwrap().clone();
        assert!(!hooked.is_empty());
        assert_eq!(hooked[0].target, Some(50));
        let broadcasted = rx.try_recv().unwrap();
        assert_eq!(broadcasted, hooked);
    }

    #[tokio::test]
    async fn position_hook_not_called_when_emit_empty() {
        let controller = Arc::new(
            BlindController::with_driver_and_positions_for_test(
                crate::config::DriverConfig::fake(),
                controller_config(),
                HashMap::from([(2, 50)]),
            )
            .await
            .unwrap(),
        );
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let hook_calls_for_hook = hook_calls.clone();
        controller.attach_position_hook(Arc::new(move |_| {
            hook_calls_for_hook.fetch_add(1, Ordering::SeqCst);
        }));
        let mut rx = controller.subscribe_positions();

        let deltas = controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        assert!(deltas.is_empty());
        assert_eq!(hook_calls.load(Ordering::SeqCst), 0);
        assert!(rx.try_recv().is_err());
    }
}
