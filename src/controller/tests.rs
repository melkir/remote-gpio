use super::*;
use crate::testing::fixtures::{fake_controller, uniform_positioning_l1_ms};
use std::collections::HashMap;
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
    let guard = controller.lock_operations_for_test().await;
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
    let guard = controller.lock_operations_for_test().await;
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
    let mut position_rx = controller.subscribe_positions();

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert!(position_rx.try_recv().is_err());
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
    let mut position_rx = controller.subscribe_positions();

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert!(position_rx.try_recv().is_err());
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
async fn position_broadcast_runs_once_per_non_empty_emit() {
    let controller = fake_controller(controller_config(), HashMap::from([(2, 100)])).await;
    let mut position_rx = controller.subscribe_positions();

    controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    let published = position_rx.recv().await.unwrap();
    assert!(!published.is_empty());
    assert_eq!(published[0].target, Some(50));
    assert!(position_rx.try_recv().is_err());
}

#[tokio::test]
async fn position_broadcast_while_operation_lock_held() {
    use crate::positioning::state::{PositionDelta, STATUS_INCREASING};

    let controller = fake_controller(controller_config(), HashMap::from([(2, 100)])).await;
    let mut position_rx = controller.subscribe_positions();
    let _guard = controller.lock_operations_for_test().await;

    controller.emit_position_deltas_for_test(&[PositionDelta {
        aid: 2,
        current: None,
        target: Some(50),
        status: Some(STATUS_INCREASING),
    }]);

    let published = tokio::time::timeout(Duration::from_millis(10), position_rx.recv())
        .await
        .expect("emit must not wait behind operation_lock")
        .unwrap();
    assert_eq!(published[0].target, Some(50));
}

#[tokio::test]
async fn position_broadcast_not_sent_when_emit_empty() {
    let controller = fake_controller(controller_config(), HashMap::from([(2, 50)])).await;
    let mut position_rx = controller.subscribe_positions();

    let deltas = controller
        .set_target_positions(vec![(2, 50)])
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert!(position_rx.try_recv().is_err());
}
