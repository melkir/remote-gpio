use crate::gpio::Channel;
use crate::remote::{Command, RemoteControl};
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::{Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{routing::get, Json, Router};
use futures_util::{
    sink::SinkExt,
    stream::{self, StreamExt},
};
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::DefaultMakeSpan;
use tower_http::trace::TraceLayer;

/// Application state shared across all routes
pub struct AppState {
    pub remote_control: Arc<RemoteControl>,
}

/// Command request structure for HTTP and WebSocket endpoints
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    command: String,
    channel: Option<Channel>,
}

/// WebSocket query parameters
#[derive(Debug, Deserialize)]
struct WsQueryParams {
    name: Option<String>,
}

/// Starts the HTTP server with all routes and middleware
pub async fn serve(shared_state: Arc<AppState>) -> Result<()> {
    let app = create_router(shared_state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:5002").await?;
    tracing::info!("Listening on http://{}", listener.local_addr()?);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Creates the router with all routes and middleware
fn create_router(shared_state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any);

    Router::new()
        .route("/channel", get(handle_channel))
        .route("/events", get(handle_events))
        .route("/command", post(handle_command))
        .route("/ws", get(ws_handler))
        .fallback(crate::embed::static_handler)
        .with_state(shared_state)
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(false)),
        )
}

/// Returns the currently-selected channel as plain text.
async fn handle_channel(State(state): State<Arc<AppState>>) -> String {
    state.remote_control.current_selection().to_string()
}

/// Streams channel selection changes as server-sent events.
async fn handle_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.remote_control.subscribe_selection();
    rx.mark_changed();
    let stream = stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        let channel = rx.borrow_and_update().to_string();
        Some((Ok(Event::default().event("selection").data(channel)), rx))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Handles command requests via HTTP
async fn handle_command(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CommandRequest>,
) -> Response {
    let CommandRequest { command, channel } = payload;
    match dispatch(&state, &command, channel).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn dispatch(state: &AppState, command: &str, channel: Option<Channel>) -> Result<(), String> {
    let (cmd, channel) = validate_command_request(command, channel)?;
    tracing::info!(command = ?cmd, channel = ?channel, "remote command received");
    if cmd == Command::Select {
        state
            .remote_control
            .execute(cmd, channel)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    if let Some(channel) = channel {
        tracing::debug!(%channel, "selecting channel");
        state
            .remote_control
            .execute(Command::Select, Some(channel))
            .await
            .map_err(|e| e.to_string())?;
    }
    state
        .remote_control
        .execute(cmd, None)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(command = ?cmd, "remote command completed");
    Ok(())
}

fn validate_command_request(
    command: &str,
    channel: Option<Channel>,
) -> Result<(Command, Option<Channel>), String> {
    let cmd = Command::from_str(command).map_err(|e| e.to_string())?;
    match (cmd, channel) {
        (Command::Prog, Some(channel)) => Ok((cmd, Some(channel))),
        (Command::Prog, None) => Err("prog requires a channel".to_string()),
        (Command::Select, channel) => Ok((cmd, channel)),
        (Command::Up | Command::Down | Command::Stop, channel) => Ok((cmd, channel)),
    }
}

/// Handles WebSocket upgrade requests
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<WsQueryParams>,
) -> impl IntoResponse {
    let client_name = params.name.unwrap_or_else(|| "anonymous".to_string());
    let port = addr.port();
    tracing::info!("[{}:{}] New WebSocket connection", client_name, port);
    ws.on_upgrade(move |socket| websocket(socket, state, client_name, port))
}

/// Manages WebSocket connections and message handling
async fn websocket(stream: WebSocket, state: Arc<AppState>, client_name: String, port: u16) {
    let (mut sink, mut stream) = stream.split();
    let mut rx_channel = state.remote_control.subscribe_selection();
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));

    // Send initial channel state.
    let selection = rx_channel.borrow().to_string();
    if sink.send(Message::Text(selection.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            // Send periodic ping to keep connection alive
            _ = ping_interval.tick() => {
                if sink.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            // Handle channel state changes.
            result = rx_channel.changed() => {
                if result.is_err() {
                    break;
                }
                let selection = rx_channel.borrow().to_string();
                if sink.send(Message::Text(selection.into())).await.is_err() {
                    break;
                }
            }
            // Handle incoming messages
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<CommandRequest>(&text) {
                            Ok(CommandRequest { command, channel }) => {
                                // Spawn command processing so LED updates aren't blocked
                                let state = state.clone();
                                let client_name = client_name.clone();
                                tokio::spawn(async move {
                                    match dispatch(&state, &command, channel).await {
                                        Ok(_) => {
                                            tracing::info!("[{}:{}] {} {:?}", client_name, port, command, channel)
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                "[{}:{}] Command execution failed: {}",
                                                client_name,
                                                port,
                                                e
                                            );
                                        }
                                    }
                                });
                            }
                            Err(_) => {
                                tracing::error!(
                                    "Invalid JSON received from client: {}:{}",
                                    client_name,
                                    port
                                );
                            }
                        }
                    }
                    Some(Ok(_)) => {} // Ignore other message types (Pong, etc.)
                    Some(Err(_)) | None => break, // Connection closed or error
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_select_with_channel() {
        assert_eq!(
            validate_command_request("select", Some(Channel::L2)).unwrap(),
            (Command::Select, Some(Channel::L2))
        );
    }

    #[test]
    fn validate_accepts_select_without_channel() {
        assert_eq!(
            validate_command_request("select", None).unwrap(),
            (Command::Select, None)
        );
    }

    #[test]
    fn validate_accepts_directional_channel() {
        assert_eq!(
            validate_command_request("up", Some(Channel::L1)).unwrap(),
            (Command::Up, Some(Channel::L1))
        );
    }

    #[test]
    fn validate_rejects_prog_without_channel() {
        let err = validate_command_request("prog", None).unwrap_err();
        assert!(err.contains("requires a channel"));
    }

    #[test]
    fn validate_accepts_prog_with_channel() {
        assert_eq!(
            validate_command_request("prog", Some(Channel::L1)).unwrap(),
            (Command::Prog, Some(Channel::L1))
        );
    }

    #[cfg(feature = "fake")]
    #[tokio::test]
    async fn dispatch_with_channel_selects_then_executes() {
        let remote_control = Arc::new(
            RemoteControl::with_driver(crate::driver::DriverConfig::default())
                .await
                .unwrap(),
        );
        let state = AppState {
            remote_control: remote_control.clone(),
        };

        dispatch(&state, "up", Some(Channel::L3)).await.unwrap();

        assert_eq!(remote_control.current_selection(), Channel::L3);
        assert_eq!(
            remote_control.operations(),
            vec![
                crate::driver::ProtocolOperation::TelisSelection(Channel::L3),
                crate::driver::ProtocolOperation::FakeCommand {
                    channel: Channel::L3,
                    command: Command::Up,
                },
            ]
        );
    }

    #[test]
    fn command_request_rejects_legacy_led_field() {
        let err = serde_json::from_str::<CommandRequest>(r#"{"command":"select","led":"L1"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
