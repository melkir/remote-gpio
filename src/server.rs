use crate::service::{BlindService, CommandError, CommandRequest};
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
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::DefaultMakeSpan;
use tower_http::trace::TraceLayer;

/// Application state shared across all routes
pub struct AppState {
    pub blinds: Arc<BlindService>,
    command_semaphore: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    pub fn new(blinds: Arc<BlindService>) -> Self {
        Self {
            blinds,
            command_semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

/// WebSocket query parameters
#[derive(Debug, Deserialize)]
struct WsQueryParams {
    name: Option<String>,
}

/// Starts the HTTP server with all routes and middleware
pub async fn serve(shared_state: Arc<AppState>, bind: &str) -> Result<()> {
    let app = create_router(shared_state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
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
    state.blinds.current_selection().to_string()
}

/// Streams channel selection changes as server-sent events.
async fn handle_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.blinds.subscribe_selection();
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
    let Ok(_permit) = state.command_semaphore.acquire().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    match execute_command(&state, payload).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn execute_command(state: &AppState, payload: CommandRequest) -> Result<(), String> {
    tracing::info!(command = %payload.command, ?payload.channel, "remote command received");
    state
        .blinds
        .dispatch_command(payload)
        .await
        .map_err(map_command_error)?;
    tracing::info!("remote command completed");
    Ok(())
}

fn map_command_error(err: CommandError) -> String {
    tracing::error!(error = %err, "remote command failed");
    err.to_string()
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
    let mut rx_channel = state.blinds.subscribe_selection();
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    let command_slots = Arc::new(tokio::sync::Semaphore::new(1));

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
                            Ok(payload) => {
                                let command = payload.command.clone();
                                let channel = payload.channel;
                                let state = state.clone();
                                let client_name = client_name.clone();
                                let command_slots = command_slots.clone();
                                tokio::spawn(async move {
                                    let Ok(_permit) = command_slots.acquire().await else {
                                        return;
                                    };
                                    match execute_command(&state, payload).await {
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
