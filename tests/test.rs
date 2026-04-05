use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::{Duration, timeout};

/// Starts an isolated server instance bound on a random available port.
///
/// Creates a fresh [`RoomMap`] with `#general` and an empty [`UserMap`],
/// then spawns a Tokio task that accepts connections in a loop.
///
/// Using port `0` lets the OS assign a free port, which prevents conflicts
/// when multiple tests run in parallel.
///
/// # Returns
///
/// The bound address as a `String` (e.g. `"127.0.0.1:54321"`),
/// ready to be passed to [`connect_as`].
async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::{RwLock, broadcast};

        let (tx, _rx) = broadcast::channel(100);
        let mut rooms_init = HashMap::new();
        rooms_init.insert("#general".to_string(), rusty_chat::Room { tx });
        let rooms: rusty_chat::RoomMap = Arc::new(RwLock::new(rooms_init));
        let users: rusty_chat::UserMap = Arc::new(RwLock::new(HashMap::new()));

        loop {
            let (socket, addr) = listener.accept().await.unwrap();
            let rooms = Arc::clone(&rooms);
            let users = Arc::clone(&users);
            tokio::spawn(async move {
                let _ = rusty_chat::handle_client(socket, rooms, users, addr.to_string()).await;
            });
        }
    });

    addr
}

/// Connects a client to the server, completes the handshake, and returns
/// the split reader/writer pair ready for use in tests.
///
/// The handshake sequence is:
/// 1. Read `"Welcome to Rusty-Chat !\n"`
/// 2. Read `"Choose a nick-name : \n"`
/// 3. Send `username`
/// 4. Read `"You are now in #general\n"` (the join confirmation)
///
/// A 50ms sleep is added after the handshake to give the server time to
/// process the join and broadcast it to other connected clients before
/// the test starts asserting.
///
/// # Returns
///
/// A `(BufReader<OwnedReadHalf>, OwnedWriteHalf)` pair for the connection.
async fn connect_as(addr: &str, username: &str) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut buf = String::new();
    reader.read_line(&mut buf).await.unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();
    buf.clear();

    writer
        .write_all(format!("{}\n", username).as_bytes())
        .await
        .unwrap();

    buf.clear();
    reader.read_line(&mut buf).await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    (reader, writer)
}

/// Reads one line from the client with a 300ms timeout.
///
/// Returns `None` if the timeout expires, if the connection is closed,
/// or if an I/O error occurs. This avoids blocking tests indefinitely
/// when no message is expected.
async fn read_line_timeout(reader: &mut BufReader<OwnedReadHalf>) -> Option<String> {
    let mut buf = String::new();
    timeout(Duration::from_millis(300), reader.read_line(&mut buf))
        .await
        .ok()?
        .ok()?;
    if buf.is_empty() { None } else { Some(buf) }
}

/// Reads lines from the client until one satisfies `predicate`, up to `attempts` tries.
///
/// Each attempt calls [`read_line_timeout`] and discards lines that do not match.
/// Returns the first matching line, or `None` if no match is found within `attempts`.
///
/// This is the primary assertion helper for tests involving async message delivery,
/// where system messages (join/leave notifications) may arrive before the expected one.
///
/// # Example
///
/// ```rust
/// // Wait for Bob's join notification, ignoring any prior system messages
/// let line = read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;
/// assert!(line.is_some());
/// ```
async fn read_until<F>(
    reader: &mut BufReader<OwnedReadHalf>,
    predicate: F,
    attempts: usize,
) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    for _ in 0..attempts {
        if let Some(line) = read_line_timeout(reader).await {
            if predicate(&line) {
                return Some(line);
            }
        }
    }
    None
}

/// Discards incoming lines from the client, up to `attempts` tries.
///
/// Stops early if [`read_line_timeout`] returns `None` (no more messages available).
/// Used to flush pending system messages (join/leave notifications) before
/// making assertions, so that leftover messages from a previous step do not
/// interfere with the next `read_until` call.
async fn drain(reader: &mut BufReader<OwnedReadHalf>, attempts: usize) {
    for _ in 0..attempts {
        if read_line_timeout(reader).await.is_none() {
            break;
        }
    }
}

// ─── Log in / Log out ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_join_broadcast_visible_to_others() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, _w2) = connect_as(&addr, "Bob").await;

    let line = read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;
    assert!(line.is_some(), "Alice should see Bob coming");
}

#[tokio::test]
async fn test_join_not_visible_to_self() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;

    let bad = read_until(&mut r1, |l| l.contains("Alice") && l.contains("joined"), 3).await;
    assert!(bad.is_none(), "Alice shouldn't see her own join: {:?}", bad);
}

#[tokio::test]
async fn test_quit_broadcast_visible_to_others() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"/quit\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Bob") && l.contains("left"), 5).await;
    assert!(line.is_some(), "Alice should see Bob leave");
}

// ─── Public messages ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_public_message_received_by_others() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"Hello everyone\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Hello everyone"), 5).await;
    assert!(line.is_some(), "Alice should receive Bob's message");
}

#[tokio::test]
async fn test_public_message_not_echoed_to_sender() {
    let addr = start_server().await;
    let (_r1, _w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r2, 5).await;

    w2.write_all(b"Hello\n").await.unwrap();

    let bad = read_until(&mut r2, |l| l.contains("Hello"), 3).await;
    assert!(
        bad.is_none(),
        "Bob shouldn't receive his own message: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_public_message_received_by_multiple_clients() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    let (_r3, mut w3) = connect_as(&addr, "Charlie").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w3.write_all(b"Hi\n").await.unwrap();

    let l1 = read_until(&mut r1, |l| l.contains("Hi"), 5).await;
    let l2 = read_until(&mut r2, |l| l.contains("Hi"), 5).await;
    assert!(l1.is_some(), "Alice should receive Charlie's message");
    assert!(l2.is_some(), "Bob should receive Charlie's message");
}

// ─── Private messages /msg ───────────────────────────────────────────────────

#[tokio::test]
async fn test_private_message_received_by_target() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"/msg Alice Hi in private\n").await.unwrap();

    let line = read_until(
        &mut r1,
        |l| l.contains("Private") && l.contains("Hi in private"),
        5,
    )
    .await;
    assert!(line.is_some(), "Alice should receive Bob's private message");
}

#[tokio::test]
async fn test_private_message_not_visible_to_third_party() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    let (mut r3, _w3) = connect_as(&addr, "Charlie").await;
    drain(&mut r1, 5).await;
    drain(&mut r3, 5).await;

    w2.write_all(b"/msg Alice Secret message\n").await.unwrap();

    let bad = read_until(&mut r3, |l| l.contains("Secret message"), 3).await;
    assert!(
        bad.is_none(),
        "Charlie shouldn't see the private message: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_private_message_not_echoed_to_sender() {
    let addr = start_server().await;
    let (_r1, _w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r2, 5).await;

    w2.write_all(b"/msg Alice Hi\n").await.unwrap();

    let bad = read_until(&mut r2, |l| l.contains("Hi"), 3).await;
    assert!(
        bad.is_none(),
        "Bob shouldn't receive his own private message: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_private_message_cross_room() {
    // Bob dans #general, Alice dans #rust — /msg doit quand même fonctionner
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("#rust"), 5).await;

    w2.write_all(b"/msg Alice Cross room message\n")
        .await
        .unwrap();

    let line = read_until(
        &mut r1,
        |l| l.contains("Private") && l.contains("Cross room message"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "Alice should receive Bob's cross-room private message"
    );
}

#[tokio::test]
async fn test_private_message_unknown_user() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"/msg Ghost Missing message\n").await.unwrap();

    let bad = read_until(&mut r1, |l| l.contains("Missing message"), 3).await;
    assert!(
        bad.is_none(),
        "Alice shouldn't receive a message for a non-existent user: {:?}",
        bad
    );

    w2.write_all(b"I'm still here\n").await.unwrap();
    let line = read_until(&mut r1, |l| l.contains("I'm still here"), 5).await;
    assert!(
        line.is_some(),
        "Bob should still be connected after an invalid /msg"
    );

    let err = read_until(
        &mut r2,
        |l| l.to_lowercase().contains("error") || l.to_lowercase().contains("not found"),
        3,
    )
    .await;
    assert!(
        err.is_some(),
        "Bob should receive an error for unknown user"
    );
}

#[tokio::test]
async fn test_private_message_usage_error() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;

    w1.write_all(b"/msg\n").await.unwrap();

    let line = read_until(
        &mut r1,
        |l| l.to_lowercase().contains("usage") || l.to_lowercase().contains("error"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "Alice should receive a usage message for malformed /msg"
    );
}

#[tokio::test]
async fn test_private_message_to_username_with_spaces() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "John Doe").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"/msg John Doe Hi buddy\n").await.unwrap();

    let line = read_until(
        &mut r1,
        |l| l.contains("Private") && l.contains("Hi buddy"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "John Doe should receive Bob's private message"
    );
}

// ─── Rooms ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_join_room_confirmation() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("#rust"), 5).await;
    assert!(
        line.is_some(),
        "Alice should receive confirmation of joining #rust"
    );
}

#[tokio::test]
async fn test_join_room_visible_to_room_members() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("#rust"), 5).await;

    w2.write_all(b"/join #rust\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;
    assert!(line.is_some(), "Alice should see Bob join #rust");
}

#[tokio::test]
async fn test_message_not_visible_across_rooms() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("#rust"), 5).await;

    w1.write_all(b"Message in #rust\n").await.unwrap();

    let bad = read_until(&mut r2, |l| l.contains("Message in #rust"), 3).await;
    assert!(
        bad.is_none(),
        "Bob in #general shouldn't see Alice's message in #rust: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_leave_returns_to_general() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("#rust"), 5).await;

    w1.write_all(b"/leave\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("#general"), 5).await;

    w1.write_all(b"Back in general\n").await.unwrap();
    let line = read_until(&mut r2, |l| l.contains("Back in general"), 5).await;
    assert!(
        line.is_some(),
        "Bob should receive Alice's message after she returns to #general"
    );
}

#[tokio::test]
async fn test_leave_from_general_rejected() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/leave\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("#general"), 5).await;
    assert!(
        line.is_some(),
        "Alice should receive an error when trying to /leave from #general"
    );
}

#[tokio::test]
async fn test_join_same_room_rejected() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/join #general\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("already"), 5).await;
    assert!(
        line.is_some(),
        "Alice should be told she's already in #general"
    );
}

#[tokio::test]
async fn test_leave_old_room_on_join() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w2.write_all(b"/join #rust\n").await.unwrap();
    read_until(&mut r2, |l| l.contains("#rust"), 5).await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/join #rust\n").await.unwrap();
    let line = read_until(&mut r2, |l| l.contains("Alice") && l.contains("joined"), 5).await;
    assert!(line.is_some(), "Bob should see Alice join #rust");

    w1.write_all(b"Hello from #rust\n").await.unwrap();
    let line = read_until(&mut r2, |l| l.contains("Hello from #rust"), 5).await;
    assert!(
        line.is_some(),
        "Bob should receive Alice's message in #rust"
    );
}

// ─── List ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_users_shows_room_members() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/list users\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Users in"), 5)
        .await
        .unwrap();
    assert!(
        line.contains("Alice"),
        "Alice should be in the list: {}",
        line
    );
    assert!(line.contains("Bob"), "Bob should be in the list: {}", line);
}

#[tokio::test]
async fn test_list_users_excludes_other_rooms() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w2.write_all(b"/join #rust\n").await.unwrap();
    drain(&mut r1, 5).await;

    w1.write_all(b"/list users\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Users in"), 5)
        .await
        .unwrap();
    assert!(
        line.contains("Alice"),
        "Alice should be in #general list: {}",
        line
    );
    assert!(
        !line.contains("Bob"),
        "Bob (in #rust) shouldn't appear in #general list: {}",
        line
    );
}

#[tokio::test]
async fn test_list_rooms_shows_all_rooms() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w2.write_all(b"/join #rust\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    w1.write_all(b"/list rooms\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Rooms:"), 5)
        .await
        .unwrap();
    assert!(
        line.contains("#general"),
        "#general should appear in rooms: {}",
        line
    );
    assert!(
        line.contains("#rust"),
        "#rust should appear in rooms: {}",
        line
    );
}

#[tokio::test]
async fn test_list_does_not_broadcast_to_others() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/list users\n").await.unwrap();

    let bad = read_until(&mut r2, |l| l.contains("Users in"), 3).await;
    assert!(
        bad.is_none(),
        "Bob shouldn't receive Alice's /list: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_list_updates_after_disconnect() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w2.write_all(b"/quit\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("left"), 5).await;

    w1.write_all(b"/list users\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Users in"), 5)
        .await
        .unwrap();
    assert!(
        !line.contains("Bob"),
        "Bob should no longer be in the list after /quit: {}",
        line
    );
    assert!(
        line.contains("Alice"),
        "Alice should still be in the list: {}",
        line
    );
}

// ─── Help ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_help_received_by_requester() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/help\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.contains("Available commands"), 5).await;
    assert!(line.is_some(), "Alice should receive the help message");

    let bad = read_until(&mut r2, |l| l.contains("Available commands"), 3).await;
    assert!(
        bad.is_none(),
        "Bob shouldn't receive Alice's /help: {:?}",
        bad
    );
}

// ─── Nick ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_nick_change_visible_to_others() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/nick Alicia\n").await.unwrap();

    let line = read_until(&mut r2, |l| l.contains("Alice") && l.contains("Alicia"), 5).await;
    assert!(line.is_some(), "Bob should see Alice's nick change");
}

#[tokio::test]
async fn test_nick_change_not_visible_to_self() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/nick Alicia\n").await.unwrap();

    let confirm = read_until(&mut r1, |l| l.contains("Alicia"), 5).await;
    assert!(
        confirm.is_some(),
        "Alice should receive nick change confirmation"
    );

    let bad = read_until(&mut r1, |l| l.contains("Alice") && l.contains("Alicia"), 3).await;
    assert!(
        bad.is_none(),
        "Alice shouldn't receive her own nick change broadcast: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_nick_already_taken() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (_r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/nick Bob\n").await.unwrap();

    let line = read_until(
        &mut r1,
        |l| l.to_lowercase().contains("taken") || l.to_lowercase().contains("error"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "Alice should be told 'Bob' is already taken"
    );
}

#[tokio::test]
async fn test_nick_empty() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    drain(&mut r1, 5).await;

    w1.write_all(b"/nick\n").await.unwrap();

    let line = read_until(&mut r1, |l| l.to_lowercase().contains("usage"), 5).await;
    assert!(
        line.is_some(),
        "Alice should receive usage message for empty /nick"
    );
}

#[tokio::test]
async fn test_nick_message_uses_new_name() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, _w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/nick Alicia\n").await.unwrap();
    read_until(&mut r2, |l| l.contains("Alicia"), 5).await;
    read_until(&mut r1, |l| l.contains("Alicia"), 5).await;

    w1.write_all(b"Hello with new nick\n").await.unwrap();

    let line = read_until(
        &mut r2,
        |l| l.contains("Alicia") && l.contains("Hello with new nick"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "Message should show new nick 'Alicia', not 'Alice'"
    );
}

#[tokio::test]
async fn test_nick_private_message_with_new_name() {
    let addr = start_server().await;
    let (mut r1, mut w1) = connect_as(&addr, "Alice").await;
    let (mut r2, mut w2) = connect_as(&addr, "Bob").await;
    drain(&mut r1, 5).await;
    drain(&mut r2, 5).await;

    w1.write_all(b"/nick Alicia\n").await.unwrap();
    read_until(&mut r2, |l| l.contains("Alicia"), 5).await;
    read_until(&mut r1, |l| l.contains("Alicia"), 5).await;

    w2.write_all(b"/msg Alicia Hey Alicia!\n").await.unwrap();

    let line = read_until(
        &mut r1,
        |l| l.contains("Private") && l.contains("Hey Alicia!"),
        5,
    )
    .await;
    assert!(
        line.is_some(),
        "Alice (now Alicia) should receive Bob's private message"
    );
}

// ─── Edge cases ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_empty_message_not_broadcast() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"\n").await.unwrap();

    let bad = read_until(&mut r1, |l| l.contains("[Bob]"), 3).await;
    assert!(
        bad.is_none(),
        "An empty message should not be broadcast: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_client_removed_after_disconnect() {
    let addr = start_server().await;
    let (mut r1, _w1) = connect_as(&addr, "Alice").await;
    let (_r2, mut w2) = connect_as(&addr, "Bob").await;
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("joined"), 5).await;

    w2.write_all(b"/quit\n").await.unwrap();
    read_until(&mut r1, |l| l.contains("Bob") && l.contains("left"), 5).await;

    let (_r3, mut w3) = connect_as(&addr, "Charlie").await;
    read_until(
        &mut r1,
        |l| l.contains("Charlie") && l.contains("joined"),
        5,
    )
    .await;

    w3.write_all(b"/msg Bob Are you there?\n").await.unwrap();

    let bad = read_until(&mut r1, |l| l.contains("Are you there?"), 3).await;
    assert!(
        bad.is_none(),
        "Bob has left, Alice shouldn't receive his private message: {:?}",
        bad
    );
}

#[tokio::test]
async fn test_concurrent_connections() {
    let addr = start_server().await;
    let mut handles = vec![];

    for i in 0..10 {
        let addr = addr.clone();
        handles.push(tokio::spawn(async move {
            let (_r, mut w) = connect_as(&addr, &format!("User{}", i)).await;
            w.write_all(format!("Hello from User{}\n", i).as_bytes())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            w.write_all(b"/quit\n").await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}
