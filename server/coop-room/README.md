# PokeDaisy co-op rooms

CO-OP MODE **ROOM**: everyone who joins the same room on the same server sees everyone else on their map. Joined
battles go to the nearest free player in the room, through the server. It runs next to CO-OP MODE **FRIENDS**, the
paired friends over the relay in `server/coop-relay`, and doesn't replace it.

```
proto/   the wire format (frames, messages, fingerprints), shared by both sides
server/  pokedaisy-coop-server: rooms, presence fan-out, direct (battle) messages
client/  pokedaisy-coop-client: the app's client, with JNI bindings (libpokedaisy_coop_client.so)
```

## Running a server

```sh
cargo run --release -p pokedaisy-coop-server            # or: docker compose up -d
```

- It listens on port **9595**: QUIC on UDP, and TCP + TLS 1.3 as the fallback for networks that block UDP. Open both.
- On the first start it creates a self-signed certificate (`server.crt` / `server.key`, key mode 600) and prints its
  **SHA-256 fingerprint** at every start. `--fingerprint` prints it and exits. Keep the files: a new certificate means
  every player has to enter the new fingerprint.
- **Access keys** go in `authorized_keys`, one per line, with `#` comments allowed. The server rereads the file when it
  changes, so you can add or revoke a key without a restart. With no file, nobody gets in.
- `--help` lists the limits (rooms, room size, clients, clients per IP, tick) and their environment variables.

## Joining from the app

Go to Settings > CO-OP > CO-OP MODE: **ROOM**, then ROOM SERVER:

| Field | What to enter |
|---|---|
| SERVER | `host` or `host:port` (the port defaults to 9595) |
| FINGERPRINT | the fingerprint the server prints (`AB:CD:…`) |
| KEY | one line of `authorized_keys` |
| ROOM | any name, 1-32 characters; case doesn't matter |

The next game you start joins the room. In-game SETTINGS shows LINK (the connection state and why it isn't up) and
PLAYERS (who's in the room).

## Security

- The client trusts only the pinned certificate (SHA-256 of the end-entity cert). It still checks real TLS signatures.
  A fingerprint that doesn't match never falls back to the other transport, and the client doesn't retry until the
  settings change.
- The key is sent only after the TLS handshake, in the Hello.
- Players see each other's trainer name, trainer ids, gender and ROM hash. That's all a room shares.
- Direct (battle) messages pass only between players on the very same ROM (its SHA-1).
- QUIC connections must answer a Retry before they count against any limit, so a forged source address can't use up
  someone's slots. Every write has a 10 s limit: a client that stops reading is dropped and can't hold the server open.

## Protocol (v1, ALPN `pokedaisy-coop/1`)

- **Frames:** `u8 kind`, `u32` big-endian length, then the payload (at most 65536 bytes).
- **Connection:** one QUIC bidirectional stream, or the TLS stream.
- **Presence:** goes up as QUIC datagrams, or as frames over TCP. Every tick (100 ms) the server sends each player
  everyone else's latest presence. A presence that hasn't been updated for 8 s (a paused game) is left out, so the
  others' apps let that hero go.
- **Liveness:** the client pings every 5 s. A server that has sent nothing for three pings counts as lost, and the
  client reconnects.
- **Messages:**
  - Client: Hello, Presence, Direct, Ping, Bye.
  - Server: Welcome (your member id, the members), Refused (a reason), Joined, Left, Presence, Direct, DirectRefused, Pong.
- **Encoding:** `proto/src/lib.rs` is the reference. The app's Kotlin decoder is `room/RoomCodec.kt`.

## Tests

```sh
cargo test                       # proto, hub, config, keys, TLS, client and the e2e suite (QUIC, TCP, fallback, pins, ...)
cargo build -p pokedaisy-coop-client --features jni
# The app's JNI end-to-end test (RoomNativeE2ETest) runs real Kotlin against the real library and server:
#   -Dpokedaisy.coopClientLib=target/debug/libpokedaisy_coop_client.so -Dpokedaisy.coopServer=target/debug/pokedaisy-coop-server
```

The Android build makes the library with cargo-ndk (`app/build.gradle.kts`, task `coopRoomNative`). It runs when the
co-op flag is on, or when you pass `-Ppokedaisy.coopNative=true`. You need `cargo install cargo-ndk` and the Android
Rust targets (`rustup target add aarch64-linux-android`, plus x86_64 / armv7 if those ABIs are on).
