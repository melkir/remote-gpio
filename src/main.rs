mod remote;
mod server;

use dotenv::dotenv;
use pusher::PusherBuilder;
use std::error::Error;
use std::sync::Arc;

use remote::{Input, RemoteControl};
use server::start_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv().ok();

    let rc = Arc::new(RemoteControl::new()?);
    let pusher = Arc::new(
        PusherBuilder::new(
            &std::env::var("PUSHER_APP_ID")?,
            &std::env::var("PUSHER_KEY")?,
            &std::env::var("PUSHER_SECRET")?,
        )
        .cluster(&std::env::var("PUSHER_CLUSTER")?)
        .finalize(),
    );

    let mut selection_rx = rc.observe(vec![
        Input::L1 as u8,
        Input::L2 as u8,
        Input::L3 as u8,
        Input::L4 as u8,
    ])?;

    // Spawn a task to handle selection changes
    let pusher_clone = pusher.clone();
    tokio::spawn(async move {
        while let Some(new_selection) = selection_rx.recv().await {
            println!("Sending selection to pusher: {}", new_selection);
            if let Err(e) = pusher_clone
                .trigger("cache-gpio", "led", new_selection)
                .await
            {
                eprintln!("Failed to send Pusher notification: {:?}", e);
            }
        }
    });

    // Start the Axum server
    start_server(rc, pusher).await;

    Ok(())
}
