//! The PokeDaisy co-op room protocol: what the app and the room server say to each other.
//!
//! One connection per player, to the server: QUIC (UDP) first, TCP + TLS when UDP doesn't get through. Both carry the same
//! [Frame]s on one reliable byte stream (QUIC: the client's first bidirectional stream). Presence goes on QUIC as unreliable
//! datagrams (a lost update is replaced by the next one); on TCP it rides the stream as frames like everything else.
//!
//! Framing on the stream: `u8 kind`, `u32 length` (big-endian), `length` bytes of payload, at most [MAX_FRAME]. A QUIC
//! datagram is one frame's kind byte and payload, no length (the datagram has one).
//!
//! Strings are a `u8` (room, game) or `u16` (auth key) byte count and that many UTF-8 bytes; numbers are big-endian.
//! Every decode refuses anything out of range, trailing bytes included: nothing is clamped.

use std::fmt;

/// The TLS ALPN both transports use; a peer that doesn't offer it is refused in the handshake.
pub const ALPN: &[u8] = b"pokedaisy-coop/1";
/// The protocol version in [ClientMsg::Hello]; the server refuses another with [RefuseReason::Version].
pub const PROTOCOL_VERSION: u16 = 1;
/// The largest frame payload on the stream.
pub const MAX_FRAME: usize = 65_536;
/// The largest [ClientMsg::Direct] / [ServerMsg::Direct] payload (a battle message; CoopWire's MAX_PAYLOAD).
pub const MAX_DIRECT: usize = 16_384;
/// The app's presence update (PresenceCodec.SIZE): opaque to the server but for its size.
pub const PRESENCE_SIZE: usize = 20;
/// Room names: 1 to 32 of `A-Z a-z 0-9 space _ - .`, compared case-insensitively (the server keys rooms by [room_key]).
pub const MAX_ROOM_NAME: usize = 32;
/// Game families (`emerald`, ...): 1 to 32 lowercase `a-z 0-9 _ -`.
pub const MAX_GAME: usize = 32;
/// Auth keys: 1 to 256 printable ASCII characters, no spaces.
pub const MAX_KEY: usize = 256;
/// Members of one room at most (ids are 1..=MAX_MEMBER_ID).
pub const MAX_MEMBER_ID: u16 = 0x7FFF;

/// A trainer as the room shows them to everyone in it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Profile {
    /// The 8 raw name bytes of the save, in the game's own charset (the app decodes them).
    pub name: [u8; 8],
    pub trainer_id: u16,
    pub secret_id: u16,
    pub female: bool,
    /// SHA-1 of the ROM: battles only go between members with the same one.
    pub rom_sha1: [u8; 20],
    /// The game family (`emerald`): a room takes one family, set by whoever opens it.
    pub game: String,
}

/// A room member: the id the server gave them for this connection, and their profile.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub id: u16,
    pub profile: Profile,
}

/// Why the server said no to a [ClientMsg::Hello].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefuseReason {
    BadKey = 1,
    Version = 2,
    RoomFull = 3,
    BadRoom = 4,
    WrongGame = 5,
    ServerFull = 6,
    BadHello = 7,
}

impl RefuseReason {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::BadKey,
            2 => Self::Version,
            3 => Self::RoomFull,
            4 => Self::BadRoom,
            5 => Self::WrongGame,
            6 => Self::ServerFull,
            7 => Self::BadHello,
            _ => return None,
        })
    }
}

/// Why a [ClientMsg::Direct] went nowhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirectRefusal {
    /// No such member in the room (they left).
    NoMember = 1,
    /// They play another ROM: battle messages only go between the same one.
    DifferentRom = 2,
    /// The sender is over the rate.
    Rate = 3,
}

impl DirectRefusal {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::NoMember,
            2 => Self::DifferentRom,
            3 => Self::Rate,
            _ => return None,
        })
    }
}

/// What a client sends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClientMsg {
    /// First and once: who, with which key, into which room.
    Hello { version: u16, key: String, room: String, profile: Profile },
    /// The player's presence (the app's 20 bytes). A datagram on QUIC, a frame on TCP.
    Presence([u8; PRESENCE_SIZE]),
    /// An app message (a battle message) to one member.
    Direct { to: u16, kind: u16, payload: Vec<u8> },
    Ping(u64),
    Bye,
}

/// What the server sends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ServerMsg {
    /// The Hello was taken: your id, how often presence comes, who is there already.
    Welcome { you: u16, tick_ms: u16, members: Vec<Member> },
    Refused(RefuseReason),
    Joined(Member),
    Left(u16),
    /// Every other member's latest presence (one tick). A datagram on QUIC, a frame on TCP.
    Presence(Vec<(u16, [u8; PRESENCE_SIZE])>),
    Direct { from: u16, kind: u16, payload: Vec<u8> },
    DirectRefused { to: u16, reason: DirectRefusal },
    Pong(u64),
}

/// A message that didn't decode.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    UnknownKind(u8),
    Truncated,
    TrailingBytes,
    Invalid(&'static str),
    TooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(k) => write!(f, "unknown message kind 0x{k:02x}"),
            Self::Truncated => write!(f, "truncated message"),
            Self::TrailingBytes => write!(f, "trailing bytes"),
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::TooLarge => write!(f, "frame too large"),
        }
    }
}

impl std::error::Error for DecodeError {}

// Kinds: client 0x01.., server 0x81...
const C_HELLO: u8 = 0x01;
const C_PRESENCE: u8 = 0x02;
const C_DIRECT: u8 = 0x03;
const C_PING: u8 = 0x05;
const C_BYE: u8 = 0x06;
const S_WELCOME: u8 = 0x81;
const S_REFUSED: u8 = 0x82;
const S_JOINED: u8 = 0x83;
const S_LEFT: u8 = 0x84;
const S_PONG: u8 = 0x85;
const S_PRESENCE: u8 = 0x86;
const S_DIRECT: u8 = 0x87;
const S_DIRECT_REFUSED: u8 = 0x88;

// ------------------------------------------------------------------------------------------------ writing

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) { self.0.push(v); }
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_be_bytes()); }
    fn u64(&mut self, v: u64) { self.0.extend_from_slice(&v.to_be_bytes()); }
    fn bytes(&mut self, b: &[u8]) { self.0.extend_from_slice(b); }
    fn str8(&mut self, s: &str) { self.u8(s.len() as u8); self.bytes(s.as_bytes()); }
    fn str16(&mut self, s: &str) { self.u16(s.len() as u16); self.bytes(s.as_bytes()); }
    fn profile(&mut self, p: &Profile) {
        self.bytes(&p.name);
        self.u16(p.trainer_id);
        self.u16(p.secret_id);
        self.u8(p.female as u8);
        self.bytes(&p.rom_sha1);
        self.str8(&p.game);
    }
    fn member(&mut self, m: &Member) { self.u16(m.id); self.profile(&m.profile); }
}

// ------------------------------------------------------------------------------------------------ reading

struct R<'a> { b: &'a [u8], at: usize }

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Self { Self { b, at: 0 } }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() - self.at < n { return Err(DecodeError::Truncated); }
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> { Ok(self.take(1)?[0]) }
    fn u16(&mut self) -> Result<u16, DecodeError> { let s = self.take(2)?; Ok(u16::from_be_bytes([s[0], s[1]])) }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        let s = self.take(8)?;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> { Ok(self.take(N)?.try_into().unwrap()) }
    fn str_n(&mut self, n: usize) -> Result<String, DecodeError> {
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| DecodeError::Invalid("utf-8"))
    }
    fn str8(&mut self) -> Result<String, DecodeError> { let n = self.u8()? as usize; self.str_n(n) }
    fn str16(&mut self) -> Result<String, DecodeError> { let n = self.u16()? as usize; self.str_n(n) }
    fn rest(&mut self) -> &'a [u8] { let s = &self.b[self.at..]; self.at = self.b.len(); s }
    fn done(&self) -> Result<(), DecodeError> {
        if self.at == self.b.len() { Ok(()) } else { Err(DecodeError::TrailingBytes) }
    }
    fn profile(&mut self) -> Result<Profile, DecodeError> {
        let p = Profile {
            name: self.arr()?,
            trainer_id: self.u16()?,
            secret_id: self.u16()?,
            female: match self.u8()? { 0 => false, 1 => true, _ => return Err(DecodeError::Invalid("female flag")) },
            rom_sha1: self.arr()?,
            game: self.str8()?,
        };
        if !valid_game(&p.game) { return Err(DecodeError::Invalid("game")); }
        Ok(p)
    }
    fn member(&mut self) -> Result<Member, DecodeError> {
        let id = self.u16()?;
        if !valid_member_id(id) { return Err(DecodeError::Invalid("member id")); }
        Ok(Member { id, profile: self.profile()? })
    }
}

// ------------------------------------------------------------------------------------------------ rules

/// A room name the server takes: 1..=32 of `A-Z a-z 0-9 space _ - .`, not only spaces.
pub fn valid_room(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ROOM_NAME && s.trim() == s && !s.trim().is_empty()
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || b" _-.".contains(&c))
}

/// How rooms are told apart: a name in lowercase (`Kanto Crew` and `kanto crew` are one room).
pub fn room_key(s: &str) -> String { s.to_ascii_lowercase() }

pub fn valid_game(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_GAME && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

pub fn valid_key(s: &str) -> bool { !s.is_empty() && s.len() <= MAX_KEY && s.bytes().all(|c| (0x21..=0x7E).contains(&c)) }

pub fn valid_member_id(id: u16) -> bool { (1..=MAX_MEMBER_ID).contains(&id) }

// ------------------------------------------------------------------------------------------------ client messages

impl ClientMsg {
    /// The kind byte and payload (a datagram for [ClientMsg::Presence] on QUIC; [frame] wraps it for the stream).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::with_capacity(64));
        match self {
            ClientMsg::Hello { version, key, room, profile } => {
                w.u8(C_HELLO); w.u16(*version); w.str16(key); w.str8(room); w.profile(profile);
            }
            ClientMsg::Presence(p) => { w.u8(C_PRESENCE); w.bytes(p); }
            ClientMsg::Direct { to, kind, payload } => { w.u8(C_DIRECT); w.u16(*to); w.u16(*kind); w.bytes(payload); }
            ClientMsg::Ping(n) => { w.u8(C_PING); w.u64(*n); }
            ClientMsg::Bye => w.u8(C_BYE),
        }
        w.0
    }

    /// A message from its kind byte and payload (a datagram, or a frame without its length).
    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R::new(b);
        let m = match r.u8()? {
            C_HELLO => {
                let version = r.u16()?;
                let key = r.str16()?;
                let room = r.str8()?;
                let profile = r.profile()?;
                if !valid_key(&key) { return Err(DecodeError::Invalid("key")); }
                if !valid_room(&room) { return Err(DecodeError::Invalid("room")); }
                ClientMsg::Hello { version, key, room, profile }
            }
            C_PRESENCE => ClientMsg::Presence(r.arr()?),
            C_DIRECT => {
                let to = r.u16()?;
                let kind = r.u16()?;
                let payload = r.rest().to_vec();
                if payload.len() > MAX_DIRECT { return Err(DecodeError::TooLarge); }
                if !valid_member_id(to) { return Err(DecodeError::Invalid("member id")); }
                ClientMsg::Direct { to, kind, payload }
            }
            C_PING => ClientMsg::Ping(r.u64()?),
            C_BYE => ClientMsg::Bye,
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.done()?;
        Ok(m)
    }
}

// ------------------------------------------------------------------------------------------------ server messages

impl ServerMsg {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::with_capacity(64));
        match self {
            ServerMsg::Welcome { you, tick_ms, members } => {
                w.u8(S_WELCOME); w.u16(*you); w.u16(*tick_ms); w.u16(members.len() as u16);
                for m in members { w.member(m); }
            }
            ServerMsg::Refused(r) => { w.u8(S_REFUSED); w.u8(*r as u8); }
            ServerMsg::Joined(m) => { w.u8(S_JOINED); w.member(m); }
            ServerMsg::Left(id) => { w.u8(S_LEFT); w.u16(*id); }
            ServerMsg::Presence(list) => {
                w.u8(S_PRESENCE); w.u16(list.len() as u16);
                for (id, p) in list { w.u16(*id); w.bytes(p); }
            }
            ServerMsg::Direct { from, kind, payload } => { w.u8(S_DIRECT); w.u16(*from); w.u16(*kind); w.bytes(payload); }
            ServerMsg::DirectRefused { to, reason } => { w.u8(S_DIRECT_REFUSED); w.u16(*to); w.u8(*reason as u8); }
            ServerMsg::Pong(n) => { w.u8(S_PONG); w.u64(*n); }
        }
        w.0
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R::new(b);
        let m = match r.u8()? {
            S_WELCOME => {
                let you = r.u16()?;
                if !valid_member_id(you) { return Err(DecodeError::Invalid("member id")); }
                let tick_ms = r.u16()?;
                let n = r.u16()? as usize;
                let mut members = Vec::with_capacity(n.min(64));
                for _ in 0..n { members.push(r.member()?); }
                ServerMsg::Welcome { you, tick_ms, members }
            }
            S_REFUSED => ServerMsg::Refused(RefuseReason::from_u8(r.u8()?).ok_or(DecodeError::Invalid("refuse reason"))?),
            S_JOINED => ServerMsg::Joined(r.member()?),
            S_LEFT => {
                let id = r.u16()?;
                if !valid_member_id(id) { return Err(DecodeError::Invalid("member id")); }
                ServerMsg::Left(id)
            }
            S_PRESENCE => {
                let n = r.u16()? as usize;
                let mut list = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    let id = r.u16()?;
                    if !valid_member_id(id) { return Err(DecodeError::Invalid("member id")); }
                    list.push((id, r.arr()?));
                }
                ServerMsg::Presence(list)
            }
            S_DIRECT => {
                let from = r.u16()?;
                if !valid_member_id(from) { return Err(DecodeError::Invalid("member id")); }
                let kind = r.u16()?;
                let payload = r.rest().to_vec();
                if payload.len() > MAX_DIRECT { return Err(DecodeError::TooLarge); }
                ServerMsg::Direct { from, kind, payload }
            }
            S_DIRECT_REFUSED => {
                let to = r.u16()?;
                let reason = DirectRefusal::from_u8(r.u8()?).ok_or(DecodeError::Invalid("direct refusal"))?;
                ServerMsg::DirectRefused { to, reason }
            }
            S_PONG => ServerMsg::Pong(r.u64()?),
            k => return Err(DecodeError::UnknownKind(k)),
        };
        r.done()?;
        Ok(m)
    }
}

// ------------------------------------------------------------------------------------------------ profiles alone

impl Profile {
    /// The profile's bytes as Hello and Member carry them (the app builds its own from these too).
    pub fn encode(&self) -> Vec<u8> { let mut w = W(Vec::with_capacity(48)); w.profile(self); w.0 }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R::new(b);
        let p = r.profile()?;
        r.done()?;
        Ok(p)
    }
}

// ------------------------------------------------------------------------------------------------ stream framing

/// [msg] (a kind byte and payload) as a stream frame: kind, u32 length of the payload, payload.
pub fn frame(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 4);
    out.push(msg[0]);
    out.extend_from_slice(&((msg.len() - 1) as u32).to_be_bytes());
    out.extend_from_slice(&msg[1..]);
    out
}

/// Splits stream bytes into messages (kind byte + payload each), keeping a partial frame for the next [push].
#[derive(Default)]
pub struct Deframer { buf: Vec<u8> }

impl Deframer {
    pub fn new() -> Self { Self::default() }

    pub fn push(&mut self, bytes: &[u8]) { self.buf.extend_from_slice(bytes); }

    /// The next whole message, `Ok(None)` while one is incomplete, an error for a frame over [MAX_FRAME].
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, DecodeError> {
        if self.buf.len() < 5 { return Ok(None); }
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_FRAME { return Err(DecodeError::TooLarge); }
        if self.buf.len() < 5 + len { return Ok(None); }
        let mut msg = Vec::with_capacity(len + 1);
        msg.push(self.buf[0]);
        msg.extend_from_slice(&self.buf[5..5 + len]);
        self.buf.drain(..5 + len);
        Ok(Some(msg))
    }
}

// ------------------------------------------------------------------------------------------------ certificate pins

/// The SHA-256 of a DER certificate: what the app pins.
pub fn fingerprint(cert_der: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(cert_der).into()
}

/// `AB:CD:...` (32 bytes, uppercase, colon separated): how the server prints a fingerprint.
pub fn fingerprint_text(fp: &[u8; 32]) -> String {
    fp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// A fingerprint as a person types it: 64 hex digits, any case, with or without `:`, spaces or a `SHA256:` prefix.
pub fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let t = s.trim();
    let t = t.strip_prefix("SHA256:").or_else(|| t.strip_prefix("sha256:")).unwrap_or(t);
    let hex: String = t.chars().filter(|c| !matches!(c, ':' | ' ' | '-')).collect();
    if hex.len() != 64 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) { return None; }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() { *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?; }
    Some(out)
}

#[cfg(test)]
mod tests;
