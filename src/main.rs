use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use colored::Colorize;
use rusty_chat::{Room, RoomMap, UserMap, handle_client};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast};

#[tokio::main]
async fn main() -> Result<()> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;

    let (tx, _rx) = broadcast::channel(100);
    let mut rooms_init = HashMap::new();
    rooms_init.insert("#general".to_string(), Room { tx });
    let rooms: RoomMap = Arc::new(RwLock::new(rooms_init));
    let users: UserMap = Arc::new(RwLock::new(HashMap::new()));

    println!("{} on {}", "Rusty-Chat launched".green().bold(), addr);

    loop {
        let (socket, addr) = listener.accept().await?;
        let rooms = rooms.clone();
        let users = users.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, rooms, users, addr.to_string()).await {
                eprintln!("Client error: {}", e);
            }
        });
    }
}
