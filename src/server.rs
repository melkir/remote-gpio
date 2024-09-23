use crate::gpio::Input;
use crate::remote::RemoteControl;
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{routing::get, Json, Router};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::DefaultMakeSpan;
use tower_http::trace::TraceLayer;

/// Application state shared across all routes
pub struct AppState {
    pub remote_control: RemoteControl,
}

/// Command request structure for HTTP and WebSocket endpoints
#[derive(Debug, Deserialize)]
struct CommandRequest {
    command: String,
    led: Option<Input>,
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
        .route("/led", get(handle_led))
        .route("/command", post(handle_command))
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new("dist"))
        .with_state(shared_state)
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(false)),
        )
}

/// Handles LED state requests
async fn handle_led(State(state): State<Arc<AppState>>) -> String {
    state.remote_control.receiver.borrow().to_string()
}

/// Handles command requests via HTTP
async fn handle_command(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CommandRequest>,
) -> Response {
    let CommandRequest { command, led } = payload;
    let rc = &state.remote_control;

    match process_command(rc, &command, led).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Processes a command and handles LED selection if specified
async fn process_command(
    rc: &RemoteControl,
    command: &str,
    led: Option<Input>,
) -> Result<(), String> {
    // Wait for specific LED if requested
    if let Some(led) = led {
        while rc.receiver.borrow().to_owned() != led {
            rc.select().await.map_err(|e| e.to_string())?;
        }
    }

    // Execute the command
    match command {
        "select" => {
            if led.is_none() {
                rc.select().await.map_err(|e| e.to_string())?;
            }
        }
        "up" => rc.up().await.map_err(|e| e.to_string())?,
        "down" => rc.down().await.map_err(|e| e.to_string())?,
        "stop" => rc.stop().await.map_err(|e| e.to_string())?,
        _ => return Err(format!("Invalid command: {}", command)),
    };

    Ok(())
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
    let (sender, mut receiver) = mpsc::channel::<String>(16);
    let mut rx_led = state.remote_control.receiver.clone();

    // Spawn task to forward messages to WebSocket
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            if sink.send(message.into()).await.is_err() {
                break;
            }
        }
    });

    // Spawn task to handle LED state updates
    let send_task_sender = sender.clone();
    let mut send_task = tokio::spawn(async move {
        // Send initial state
        let selection = rx_led.borrow().to_string();
        if send_task_sender.send(selection).await.is_err() {
            return;
        }

        // Handle subsequent updates
        while rx_led.changed().await.is_ok() {
            let selection = rx_led.borrow().to_string();
            if send_task_sender.send(selection).await.is_err() {
                break;
            }
        }
    });

    // Spawn task to handle incoming WebSocket messages
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = stream.next().await {
            if text == "ping" {
                tracing::debug!("[{}:{}] received ping", client_name, port);
                if sender.send("pong".into()).await.is_err() {
                    break;
                }
                continue;
            }

            let rc = &state.remote_control;
            match serde_json::from_str::<CommandRequest>(&text) {
                Ok(CommandRequest { command, led }) => {
                    match process_command(rc, &command, led).await {
                        Ok(_) => {
                            tracing::info!("[{}:{}] {} {:?}", client_name, port, command, led)
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
                }
                Err(_) => {
                    tracing::error!(
                        "Invalid JSON received from client: {}:{}",
                        client_name,
                        port
                    );
                }
            };
        }
    });

    // Wait for either task to complete and abort the other
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
}
