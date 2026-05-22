use super::test_support::{fake_controller, uniform_positioning_l1_ms};
use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::{timeout, Duration};

fn controller_config() -> crate::config::PositioningOptions {
    crate::config::PositioningOptions::default()
}

fn attach_listener(
    controller: &BlindController,
) -> (Arc<AtomicUsize>, Arc<Mutex<Vec<PositionDelta>>>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let calls_for_listener = calls.clone();
    let captured_for_listener = captured.clone();
    controller.attach_position_listener(Arc::new(move |deltas| {
        calls_for_listener.fetch_add(1, Ordering::SeqCst);
        *captured_for_listener.lock().unwrap() = deltas.to_vec();
    }));
    (calls, captured)
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
    let controller =
        fake_controller(uniform_positioning_l1_ms(50), HashMap::from([(2, 100)])).await;
    let guard = controller.operation_lock.lock().await;
    let pending_controller = controller.clone();

    let operation =
        tokio::spawn(async move { pending_controller.set_target_positions(vec![(2, 50)]).await });

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
    let controller = fake_controller(uniform_positioning_l1_ms(2), HashMap::from([(2, 100)])).await;

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
    let controller = fake_controller(controller_config(), HashMap::from([(2, 50)])).await;
    let (listener_calls, _) = attach_listener(&controller);

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert_eq!(listener_calls.load(Ordering::SeqCst), 0);
    assert_eq!(controller.operations(), Vec::new());
    assert_eq!(controller.position_for_aid(2).await.current, 50);
    assert_eq!(controller.position_for_aid(2).await.target, 50);
}

#[tokio::test]
async fn target_position_matching_pending_target_is_noop() {
    let controller =
        fake_controller(uniform_positioning_l1_ms(50), HashMap::from([(2, 100)])).await;
    controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();
    let (listener_calls, _) = attach_listener(&controller);

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert_eq!(listener_calls.load(Ordering::SeqCst), 0);
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
async fn position_listener_runs_once_per_non_empty_emit() {
    let controller = fake_controller(controller_config(), HashMap::from([(2, 100)])).await;
    let (listener_calls, captured) = attach_listener(&controller);

    controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert_eq!(listener_calls.load(Ordering::SeqCst), 1);
    let hooked = captured.lock().unwrap().clone();
    assert!(!hooked.is_empty());
    assert_eq!(hooked[0].target, Some(50));
}

#[tokio::test]
async fn position_listener_not_called_when_emit_empty() {
    let controller = fake_controller(controller_config(), HashMap::from([(2, 50)])).await;
    let (listener_calls, _) = attach_listener(&controller);

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert_eq!(listener_calls.load(Ordering::SeqCst), 0);
}
