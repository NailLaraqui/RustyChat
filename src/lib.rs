//! # Rusty-Chat
//!
//! Async multi-room TCP chat server built with Tokio.
//!
//! ## Architecture
//!
//! The server revolves around two shared data structures:
//!
//! - [`RoomMap`] — maps room names to their [`Room`] (each room has its own broadcast channel)
//! - [`UserMap`] — maps nick-names to the room they are currently in
//!
//! Each connected client runs in its own Tokio task via [`handle_client`].
//! Messages are routed through per-room [`broadcast`] channels,
//! ensuring that messages never leak across rooms.
//!
//! ## Available commands
//!
//! | Command | Description |
//! |---------|-------------|
//! | `/join <room>` | Join or create a room |
//! | `/leave` | Return to `#general` |
//! | `/msg <nick> <text>` | Send a private message (supports nicks with spaces) |
//! | `/nick <new_nick>` | Change your nick-name |
//! | `/list users` | List users in the current room |
//! | `/list rooms` | List all existing rooms |
//! | `/help` | Show available commands |
//! | `/quit` | Disconnect |

use colored::Colorize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{RwLock, broadcast};

/// A chat message routed through the broadcast channels.
///
/// # Fields
///
/// - `sender` — display name shown to recipients (e.g. `"Alice"` or `"System"`)
/// - `origin` — nick-name of the user who triggered the message; used to prevent
///   echoing messages back to their author, even for system messages
///   (e.g. `"Alice joined #general"` has `origin: "Alice"` so Alice doesn't see it)
/// - `target` — if `Some(nick)`, the message is private and only delivered to that user;
///   if `None`, the message is public and delivered to all room members
/// - `content` — the message body
#[derive(Clone)]
pub struct ChatMessage {
    pub sender: String,
    pub origin: String,
    pub target: Option<String>,
    pub content: String,
}

/// A chat room with its own isolated broadcast channel.
///
/// Each room maintains a [`broadcast::Sender`] so that messages sent in one room
/// are never visible to users in other rooms.
/// Rooms persist even when empty — they are created on first `/join` and
/// never deleted.
pub struct Room {
    pub tx: broadcast::Sender<ChatMessage>,
}

/// Shared map of all existing rooms.
///
/// Key: room name (e.g. `"#general"`, `"#rust"`)
/// Value: [`Room`] holding the broadcast channel for that room
///
/// Wrapped in `Arc<RwLock<_>>` for safe concurrent access across Tokio tasks.
pub type RoomMap = Arc<RwLock<HashMap<String, Room>>>;

/// Shared map of all connected users and their current room.
///
/// Key: nick-name (e.g. `"Alice"`)
/// Value: name of the room the user is currently in (e.g. `"#general"`)
///
/// Wrapped in `Arc<RwLock<_>>` for safe concurrent access across Tokio tasks.
/// Used by `/msg` to route private messages across rooms.
pub type UserMap = Arc<RwLock<HashMap<String, String>>>;

/// Handles a single connected client for its entire session.
///
/// This function is spawned as a Tokio task for each accepted TCP connection.
/// It drives the full client lifecycle:
///
/// 1. Prompts for a nick-name
/// 2. Inserts the user into [`UserMap`] and joins `#general` automatically
/// 3. Subscribes to the room's broadcast channel
/// 4. Enters the main `select!` loop, concurrently:
///    - Reading lines from the client and dispatching commands
///    - Receiving broadcast messages and forwarding them to the client
/// 5. On exit (`/quit` or disconnection), removes the user from [`UserMap`]
///    and broadcasts a leave message
///
/// # Arguments
///
/// - `socket` — the accepted [`TcpStream`] for this client
/// - `rooms` — shared [`RoomMap`] for all rooms
/// - `users` — shared [`UserMap`] tracking which room each user is in
/// - `ip_addr` — client IP address, used for server-side logging
///
/// # Errors
///
/// Returns an error if any I/O operation on the socket fails unexpectedly.
///
/// # Command parsing
///
/// Commands are matched in order with `starts_with`. The `/msg` command
/// supports nick-names containing spaces by searching [`UserMap`] for the
/// longest matching prefix in the input.
///
/// # Broadcast filtering
///
/// A received [`ChatMessage`] is forwarded to the client only if **both**
/// conditions are met:
/// - `msg.origin != username` — the client did not trigger this message
/// - `msg.target.is_none() || msg.target == Some(username)` — the message is
///   public or explicitly addressed to this client
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

/// Broadcasts a [`ChatMessage`] to all subscribers of a given room.
///
/// Acquires a read lock on [`RoomMap`], looks up the room by name,
/// and sends the message through its [`broadcast::Sender`].
/// If the room does not exist, the message is silently dropped.
///
/// Errors from [`broadcast::Sender::send`] (e.g. no active receivers)
/// are intentionally ignored — it is normal for a room to have no listeners.
///
/// # Arguments
///
/// - `rooms` — shared [`RoomMap`]
/// - `room_name` — name of the target room
/// - `msg` — the message to broadcast
async fn broadcast_to_room(rooms: &RoomMap, room_name: &str, msg: ChatMessage) {
    let map = rooms.read().await;
    if let Some(room) = map.get(room_name) {
        let _ = room.tx.send(msg);
    }
}
