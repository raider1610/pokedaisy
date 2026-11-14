//! The app's door into the client: `com.pokedaisy.app.companion.coop.room.RoomNative` (Kotlin). Clients are kept in a
//! table by a handle (never a raw pointer), so a stale or doubled handle from Kotlin does nothing instead of crashing.
//! Nothing here blocks the calling thread except [poll]'s optional wait.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use jni::objects::{JByteArray, JObject, JString};
use jni::sys::{jboolean, jbyteArray, jint, jlong, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;

use crate::{proto, Client, Config, Transports};

fn table() -> &'static Mutex<HashMap<i64, Arc<Client>>> {
    static T: OnceLock<Mutex<HashMap<i64, Arc<Client>>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT: AtomicI64 = AtomicI64::new(1);

fn client(h: jlong) -> Option<Arc<Client>> { table().lock().unwrap().get(&h).cloned() }

fn string(env: &mut JNIEnv, s: &JString) -> Option<String> { env.get_string(s).ok().map(|s| s.into()) }

fn bytes(env: &mut JNIEnv, b: &JByteArray) -> Option<Vec<u8>> {
    if b.is_null() { return None; }
    env.convert_byte_array(b).ok()
}

/// Starts a connection: a handle > 0, or 0 when an argument is bad (no connection made). [transports]: 0 auto (QUIC,
/// then TCP), 1 QUIC only, 2 TCP only. [profile] is the protocol's profile bytes; [pin] the certificate's 32-byte SHA-256.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_open<'l>(
    mut env: JNIEnv<'l>,
    _this: JObject<'l>,
    host: JString<'l>,
    quic_port: jint,
    tcp_port: jint,
    pin: JByteArray<'l>,
    key: JString<'l>,
    room: JString<'l>,
    profile: JByteArray<'l>,
    transports: jint,
) -> jlong {
    let ok = (|| {
        let host = string(&mut env, &host)?;
        let key = string(&mut env, &key)?;
        let room = string(&mut env, &room)?;
        let pin: [u8; 32] = bytes(&mut env, &pin)?.try_into().ok()?;
        let profile = proto::Profile::decode(&bytes(&mut env, &profile)?).ok()?;
        let port = |p: jint| u16::try_from(p).ok().filter(|p| *p != 0);
        if host.is_empty() || !proto::valid_key(&key) || !proto::valid_room(&room) { return None; }
        let mut cfg = Config::new(&host, port(quic_port)?, pin, &key, &room, profile);
        cfg.tcp_port = port(tcp_port)?;
        cfg.transports = match transports { 1 => Transports::QuicOnly, 2 => Transports::TcpOnly, _ => Transports::Auto };
        Some(cfg)
    })();
    let Some(cfg) = ok else { return 0 };
    let h = NEXT.fetch_add(1, Ordering::SeqCst);
    table().lock().unwrap().insert(h, Arc::new(Client::connect(cfg)));
    h
}

/// The next event's bytes (Event::encode), waiting up to [wait_ms] for one; null when none (or a stale handle). After
/// the Closed event the handle is forgotten.
#[no_mangle]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_poll<'l>(
    env: JNIEnv<'l>,
    _this: JObject<'l>,
    handle: jlong,
    wait_ms: jint,
) -> jbyteArray {
    let Some(c) = client(handle) else { return std::ptr::null_mut() };
    let e = if wait_ms > 0 { c.wait(Duration::from_millis(wait_ms as u64)) } else { c.poll() };
    let Some(e) = e else { return std::ptr::null_mut() };
    if matches!(e, crate::Event::Closed(_)) { table().lock().unwrap().remove(&handle); }
    match env.byte_array_from_slice(&e.encode()) {
        Ok(a) => a.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// The player's presence (20 bytes); false for a stale handle or a wrong size.
#[no_mangle]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_sendPresence<'l>(
    mut env: JNIEnv<'l>,
    _this: JObject<'l>,
    handle: jlong,
    presence: JByteArray<'l>,
) -> jboolean {
    let Some(c) = client(handle) else { return JNI_FALSE };
    let Some(p) = bytes(&mut env, &presence).and_then(|b| <[u8; proto::PRESENCE_SIZE]>::try_from(b).ok()) else { return JNI_FALSE };
    if c.send_presence(p) { JNI_TRUE } else { JNI_FALSE }
}

/// A direct (battle) message to member [to]; false for a stale handle or a bad argument.
#[no_mangle]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_sendDirect<'l>(
    mut env: JNIEnv<'l>,
    _this: JObject<'l>,
    handle: jlong,
    to: jint,
    kind: jint,
    payload: JByteArray<'l>,
) -> jboolean {
    let Some(c) = client(handle) else { return JNI_FALSE };
    let (Ok(to), Ok(kind)) = (u16::try_from(to), u16::try_from(kind)) else { return JNI_FALSE };
    let Some(p) = bytes(&mut env, &payload) else { return JNI_FALSE };
    if c.send_direct(to, kind, p) { JNI_TRUE } else { JNI_FALSE }
}

/// Bye and close; the Closed event still comes through [poll].
#[no_mangle]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_close<'l>(_env: JNIEnv<'l>, _this: JObject<'l>, handle: jlong) {
    if let Some(c) = client(handle) { c.close(); }
}

/// The protocol's fingerprint parser, for the settings field: 32 bytes, or null for text that isn't one.
#[no_mangle]
pub extern "system" fn Java_com_pokedaisy_app_companion_coop_room_RoomNative_parseFingerprint<'l>(
    mut env: JNIEnv<'l>,
    _this: JObject<'l>,
    text: JString<'l>,
) -> jbyteArray {
    let Some(t) = string(&mut env, &text) else { return std::ptr::null_mut() };
    let Some(fp) = proto::parse_fingerprint(&t) else { return std::ptr::null_mut() };
    env.byte_array_from_slice(&fp).map(|a| a.into_raw()).unwrap_or(std::ptr::null_mut())
}
