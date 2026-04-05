use colored::Colorize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{RwLock, broadcast};

#[derive(Clone)]
pub struct ChatMessage {
    pub sender: String,
    pub origin: String,
    pub target: Option<String>,
    pub content: String,
}

pub struct Room {
    pub tx: broadcast::Sender<ChatMessage>,
}

// room_name -> Room
pub type RoomMap = Arc<RwLock<HashMap<String, Room>>>;

// nick-name -> current room_name
pub type UserMap = Arc<RwLock<HashMap<String, String>>>;

pub async fn handle_client(
    socket: TcpStream,
    rooms: RoomMap,
    users: UserMap,
    ip_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    writer
        .write_all(b"Welcome to Rusty-Chat !\nChoose a nick-name : \n")
        .await?;

    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let mut username = line.trim().to_string();
    line.clear();

    let mut current_room = "#general".to_string();
    users
        .write()
        .await
        .insert(username.clone(), current_room.clone());

    let mut rx = {
        let map = rooms.read().await;
        map[&current_room].tx.subscribe()
    };

    broadcast_to_room(
        &rooms,
        &current_room,
        ChatMessage {
            sender: "System".to_string(),
            origin: username.clone(),
            target: None,
            content: format!("{} joined {}", username, current_room)
                .cyan()
                .to_string(),
        },
    )
    .await;

    writer
        .write_all(format!("You are now in {}\n", current_room).as_bytes())
        .await?;

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                if result? == 0 { break; }

                let input = line.trim().to_string();

                if input.starts_with("/quit") {
                    break;

                } else if input.starts_with("/join") {
                    let room_name = input
                        .strip_prefix("/join").unwrap()
                        .trim()
                        .to_string();

                    if room_name.is_empty() {
                        writer.write_all(b"Usage: /join <room>\n").await?;
                        line.clear();
                        continue;
                    }

                    if room_name == current_room {
                        writer
                            .write_all(format!("You are already in {}\n", current_room).as_bytes())
                            .await?;
                        line.clear();
                        continue;
                    }

                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: "System".to_string(),
                        origin: username.clone(),
                        target: None,
                        content: format!("{} left {}", username, current_room).red().to_string(),
                    }).await;

                    {
                        let mut map = rooms.write().await;
                        map.entry(room_name.clone()).or_insert_with(|| {
                            let (tx, _) = broadcast::channel(100);
                            Room { tx }
                        });
                    }

                    rx = {
                        let map = rooms.read().await;
                        map[&room_name].tx.subscribe()
                    };

                    current_room = room_name.clone();
                    users.write().await.insert(username.clone(), current_room.clone());

                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: "System".to_string(),
                        origin: username.clone(),
                        target: None,
                        content: format!("{} joined {}", username, current_room).cyan().to_string(),
                    }).await;

                    writer
                        .write_all(format!("You are now in {}\n", current_room).as_bytes())
                        .await?;

                } else if input.starts_with("/leave") {
                    if current_room == "#general" {
                        writer.write_all(b"You are already in #general\n").await?;
                        line.clear();
                        continue;
                    }

                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: "System".to_string(),
                        origin: username.clone(),
                        target: None,
                        content: format!("{} left {}", username, current_room).red().to_string(),
                    }).await;

                    rx = {
                        let map = rooms.read().await;
                        map["#general"].tx.subscribe()
                    };

                    current_room = "#general".to_string();
                    users.write().await.insert(username.clone(), current_room.clone());

                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: "System".to_string(),
                        origin: username.clone(),
                        target: None,
                        content: format!("{} joined {}", username, current_room).cyan().to_string(),
                    }).await;

                    writer.write_all(b"You are back in #general\n").await?;

                } else if input.starts_with("/list") {
                    let arg = input.strip_prefix("/list").unwrap().trim().to_string();

                    match arg.as_str() {
                        "rooms" => {
                            let map = rooms.read().await;
                            let list = map.keys().cloned().collect::<Vec<_>>().join(", ");
                            drop(map);
                            writer.write_all(format!("Rooms: {}\n", list).as_bytes()).await?;
                        }
                        "users" | "" => {
                            let user_map = users.read().await;
                            let list = user_map.iter()
                                .filter(|(_, room)| *room == &current_room)
                                .map(|(name, _)| name.clone())
                                .collect::<Vec<_>>()
                                .join(", ");
                            drop(user_map);
                            writer
                                .write_all(format!("Users in {}: {}\n", current_room, list).as_bytes())
                                .await?;
                        }
                        _ => {
                            writer.write_all(b"Usage: /list users | /list rooms\n").await?;
                        }
                    }

                } else if input.starts_with("/msg") {
                    let after_cmd = input.strip_prefix("/msg").unwrap_or("").trim().to_string();
                    let user_map = users.read().await;

                    let found = user_map.keys()
                        .filter(|name| {
                            after_cmd.starts_with(name.as_str())
                                && after_cmd[name.len()..].starts_with(' ')
                        })
                        .max_by_key(|name| name.len())
                        .cloned();

                    let target_room = found.as_ref().and_then(|name| user_map.get(name)).cloned();
                    drop(user_map);

                    if let (Some(dest), Some(room)) = (found, target_room) {
                        let content = input
                            .strip_prefix("/msg").unwrap()
                            .trim()[dest.len()..]
                            .trim()
                            .to_string();

                        broadcast_to_room(&rooms, &room, ChatMessage {
                            sender: username.clone(),
                            origin: username.clone(),
                            target: Some(dest),
                            content,
                        }).await;
                    } else {
                        writer.write_all(b"Error: User not found\n").await?;
                    }

                } else if input.starts_with("/nick") {
                    let new_nick = input.strip_prefix("/nick").unwrap().trim().to_string();

                    if new_nick.is_empty() {
                        writer.write_all(b"Usage: /nick <new-nick>\n").await?;
                        line.clear();
                        continue;
                    }

                    {
                        let user_map = users.read().await;
                        if user_map.contains_key(&new_nick) {
                            writer
                                .write_all(format!("Error: '{}' is already taken\n", new_nick).as_bytes())
                                .await?;
                            line.clear();
                            continue;
                        }
                    }

                    {
                        let mut user_map = users.write().await;
                        user_map.remove(&username);
                        user_map.insert(new_nick.clone(), current_room.clone());
                    }

                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: "System".to_string(),
                        origin: new_nick.clone(),
                        target: None,
                        content: format!("{} is now known as {}", username, new_nick).yellow().to_string(),
                    }).await;

                    username = new_nick;
                    writer.
                        write_all(format!("You are now known as {}\n", username).as_bytes())
                        .await?;
                } else if input.starts_with("/help") {
                    writer.write_all(b"Available commands:\n\
                        /join <room>        - Join or create a room\n\
                        /leave              - Return to #general\n\
                        /msg <nick> <text>  - Send a private message\n\
                        /nick <new_nick>    - Set a new nick-name\n\
                        /list users         - List users in current room\n\
                        /list rooms         - List all rooms\n\
                        /help               - Show this help\n\
                        /quit               - Disconnect\n"
                    ).await?;
                } else {
                    broadcast_to_room(&rooms, &current_room, ChatMessage {
                        sender: username.clone(),
                        origin: username.clone(),
                        target: None,
                        content: input.clone(),
                    }).await;
                }

                line.clear();
            }

            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        let is_origin = msg.origin == username;
                        let for_me = msg.target.as_ref() == Some(&username);
                        let is_public = msg.target.is_none();

                        if !is_origin && (is_public || for_me) {
                            let prefix = if for_me { "[Private] ".magenta() } else { "".normal() };
                            let formatted = format!(
                                "{}[{}]: {}\n",
                                prefix,
                                msg.sender.yellow(),
                                msg.content
                            );
                            writer.write_all(formatted.as_bytes()).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        writer
                            .write_all(format!("--- You missed {} messages ---\n", n).as_bytes())
                            .await?;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    users.write().await.remove(&username);
    broadcast_to_room(
        &rooms,
        &current_room,
        ChatMessage {
            sender: "System".to_string(),
            origin: username.clone(),
            target: None,
            content: format!("{} left the chat.", username).red().to_string(),
        },
    )
    .await;

    println!("{} disconnected", ip_addr);
    Ok(())
}

async fn broadcast_to_room(rooms: &RoomMap, room_name: &str, msg: ChatMessage) {
    let map = rooms.read().await;
    if let Some(room) = map.get(room_name) {
        let _ = room.tx.send(msg);
    }
}
