# Rusty-Chat

Async multi-room TCP chat server written in Rust, built with Tokio.

## Features

- Multi-room support — create and join rooms on the fly
- Public and private messaging
- Nick-name changes during a session
- Private messages work across rooms
- Full integration test suite (35+ tests)

## Getting started

### Prerequisites

- Rust 1.75+
- Cargo

### Build & run

```bash
git clone https://github.com/NailLaraqui/RustyChat.git
cd rusty-chat
cargo run
```

The server listens on `127.0.0.1:8080` by default.

### Connect

Any TCP client works — `netcat` is the simplest:

```bash
nc 127.0.0.1 8080
```

You will be prompted for a nick-name. Once chosen, you are automatically placed in `#general`.

## Commands

| Command | Description |
|---------|-------------|
| `/join <room>` | Join or create a room |
| `/leave` | Return to `#general` |
| `/msg <nick> <message>` | Send a private message (nick-names with spaces are supported) |
| `/nick <new-nick>` | Change your nick-name |
| `/list users` | List users in the current room |
| `/list rooms` | List all existing rooms |
| `/help` | Show available commands |
| `/quit` | Disconnect |

## Example session

```
$ nc 127.0.0.1 8080
Welcome to Rusty-Chat !
Choose a nick-name :
Alice
You are now in #general
[System]: Bob joined #general
/join #rust
You are now in #rust
/msg Bob Hey, come to #rust !
/list users
Users in #rust: Alice
/nick Ali
You are now known as Ali
/quit
```

## Running the tests

```bash
cargo test
```

Each test starts its own server instance on a random port, so all tests run safely in parallel.

## Project structure

```
src/
├── lib.rs      # Core logic — handle_client, RoomMap, UserMap, ChatMessage
└── main.rs     # Entry point — TCP listener, shared state initialization
tests/
└── test.rs     # Integration tests
```

## Dependencies

| Crate | Usage |
|-------|-------|
| `tokio` | Async runtime, TCP, broadcast channels |
| `colored` | Terminal colors for server-side output |
| `anyhow` | Error handling in main |

## License

MIT
