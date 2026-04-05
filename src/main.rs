//! # Rusty-Chat — Server Entry Point
//!
//! Binds a TCP listener on `127.0.0.1:8080`, initializes the shared state,
//! and spawns a Tokio task for each incoming connection.
//!
//! ## Startup sequence
//!
//! 1. Bind the TCP listener
//! 2. Create the `#general` room with its broadcast channel
//! 3. Initialize the empty [`UserMap`]
//! 4. Accept connections in a loop, cloning the shared state for each task

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use colored::Colorize;
use rusty_chat::{Room, RoomMap, UserMap, handle_client};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast};

/// Entry point of the Rusty-Chat server.
///
/// # Initialization
///
/// The server creates a single default room `#general` at startup.
/// All users are automatically joined to this room upon connection.
/// The `#general` room is never removed, even when empty.
///
/// A [`broadcast::channel`] with a capacity of `100` is created for `#general`.
/// The initial receiver `_rx` is immediately dropped — this is intentional:
/// the channel stays alive as long as the [`broadcast::Sender`] exists inside
/// [`Room`], and each client subscribes independently via [`broadcast::Sender::subscribe`].
///
/// # Connection handling
///
/// For each accepted connection, [`Arc::clone`] is called on both [`RoomMap`]
/// and [`UserMap`] to give the spawned task shared ownership without copying
/// the underlying data.
/// Each task runs [`handle_client`] and logs any returned error to stderr
/// before exiting silently, so that one failing client never brings down the server.
///
/// # Errors
///
/// Returns an error if the TCP listener fails to bind or if [`TcpListener::accept`]
/// encounters an unrecoverable error.
#[tokio::main]
async fn main() -> Result<()> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;

    // Create #general with its broadcast channel.
    // _rx is dropped immediately — each client subscribes independently.
    let (tx, _rx) = broadcast::channel(100);
    let mut rooms_init = HashMap::new();
    rooms_init.insert("#general".to_string(), Room { tx });
    let rooms: RoomMap = Arc::new(RwLock::new(rooms_init));

    // Initially empty — users are inserted on connection and removed on disconnect.
    let users: UserMap = Arc::new(RwLock::new(HashMap::new()));

    println!("{} on {}", "Rusty-Chat launched".green().bold(), addr);

    loop {
        let (socket, addr) = listener.accept().await?;

        // Clone the Arcs — cheap reference count increment, not a data copy.
        let rooms = rooms.clone();
        let users = users.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, rooms, users, addr.to_string()).await {
                eprintln!("Client error: {}", e);
            }
        });
    }
}
