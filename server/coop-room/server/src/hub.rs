//! The rooms, with no sockets in them: who is where, their latest presence, the limits and the routing. Every
//! connection talks to the [Hub] through a [Session] and hears back through its [Out] channel; a member the hub drops
//! (a reader that stopped) loses that channel, which closes its connection.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pokedaisy_coop_proto::{self as proto, DirectRefusal, Member, Profile, RefuseReason, ServerMsg, PRESENCE_SIZE};
use tokio::sync::mpsc;

/// What a connection is told to send: a reliable message (the stream), or a presence batch (a datagram where it can).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    Reliable(Vec<u8>),
    Presence(Vec<u8>),
}

/// Messages queued for one member before the hub calls it a stalled reader and drops it.
pub const OUT_QUEUE: usize = 1_024;

/// A member whose presence hasn't changed hands for this long (the game paused, the app in the background) is left out
/// of the ticks, so the others' apps let their hero go (the app's own limit is 10 s) instead of drawing it frozen.
pub const PRESENCE_STALE: std::time::Duration = std::time::Duration::from_secs(8);

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_rooms: usize,
    pub room_size: usize,
    pub max_clients: usize,
    pub max_clients_per_ip: usize,
    pub direct_per_sec: f64,
    pub presence_per_sec: f64,
    pub tick_ms: u16,
}

impl From<&crate::config::Config> for Limits {
    fn from(c: &crate::config::Config) -> Self {
        Self {
            max_rooms: c.max_rooms,
            room_size: c.room_size,
            max_clients: c.max_clients,
            max_clients_per_ip: c.max_clients_per_ip,
            direct_per_sec: c.direct_per_sec,
            presence_per_sec: c.presence_per_sec,
            tick_ms: c.tick.as_millis() as u16,
        }
    }
}

/// A member in a room, as one connection holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub room: String,
    pub id: u16,
}

pub struct Hub {
    limits: Limits,
    inner: Mutex<Inner>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
}

#[derive(Default)]
struct Inner {
    rooms: HashMap<String, Room>,
    clients: usize,
    per_ip: HashMap<IpAddr, usize>,
}

struct Room {
    game: String,
    members: BTreeMap<u16, Slot>,
    next_id: u16,
}

struct Slot {
    profile: Profile,
    tx: mpsc::Sender<Out>,
    presence: Option<[u8; PRESENCE_SIZE]>,
    presence_at: Instant,
    direct: Bucket,
    presence_rate: Bucket,
}

/// A token bucket: [rate] per second, one second of burst.
struct Bucket { tokens: f64, at: Instant, rate: f64 }

impl Bucket {
    fn new(rate: f64, now: Instant) -> Self { Self { tokens: rate, at: now, rate } }
    fn take(&mut self, now: Instant) -> bool {
        let secs = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + secs * self.rate).min(self.rate);
        if self.tokens < 1.0 { return false; }
        self.tokens -= 1.0;
        true
    }
}

/// One admitted connection (counted against the server's and its address's limits) until dropped.
pub struct Admission {
    hub: Arc<Hub>,
    ip: IpAddr,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut g = self.hub.inner.lock().unwrap();
        g.clients = g.clients.saturating_sub(1);
        if let Some(n) = g.per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 { g.per_ip.remove(&self.ip); }
        }
    }
}

/// Counts and sizes, for the log and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats { pub rooms: usize, pub members: usize, pub clients: usize }

impl Hub {
    pub fn new(limits: Limits) -> Arc<Self> { Self::with_clock(limits, Instant::now) }

    pub fn with_clock(limits: Limits, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { limits, inner: Mutex::new(Inner::default()), clock: Box::new(clock) })
    }

    pub fn limits(&self) -> &Limits { &self.limits }

    /// A new connection from [ip]: None when the server or that address is at its limit.
    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Admission> {
        let mut g = self.inner.lock().unwrap();
        if g.clients >= self.limits.max_clients { return None; }
        let n = g.per_ip.get(&ip).copied().unwrap_or(0);
        if n >= self.limits.max_clients_per_ip { return None; }
        g.clients += 1;
        g.per_ip.insert(ip, n + 1);
        Some(Admission { hub: Arc::clone(self), ip })
    }

    /// [profile] into [room] (made if new): the session and its Welcome; everyone already there hears Joined.
    pub fn join(&self, room: &str, profile: Profile, tx: mpsc::Sender<Out>) -> Result<(Session, ServerMsg), RefuseReason> {
        if !proto::valid_room(room) { return Err(RefuseReason::BadRoom); }
        if !proto::valid_game(&profile.game) { return Err(RefuseReason::BadHello); }
        let key = proto::room_key(room);
        let now = (self.clock)();
        let mut g = self.inner.lock().unwrap();
        let rooms_now = g.rooms.len();
        let r = match g.rooms.get_mut(&key) {
            Some(r) => r,
            None => {
                if rooms_now >= self.limits.max_rooms { return Err(RefuseReason::ServerFull); }
                g.rooms.entry(key.clone()).or_insert_with(|| Room { game: profile.game.clone(), members: BTreeMap::new(), next_id: 1 })
            }
        };
        if r.game != profile.game { return Err(RefuseReason::WrongGame); }
        if r.members.len() >= self.limits.room_size { return Err(RefuseReason::RoomFull); }
        let id = r.free_id();
        let others: Vec<Member> = r.members.iter().map(|(&id, s)| Member { id, profile: s.profile.clone() }).collect();
        let joined = ServerMsg::Joined(Member { id, profile: profile.clone() }).encode();
        let mut stalled = Vec::new();
        for (&mid, s) in &r.members {
            if s.tx.try_send(Out::Reliable(joined.clone())).is_err() { stalled.push(mid); }
        }
        r.members.insert(id, Slot {
            profile,
            tx,
            presence: None,
            presence_at: now,
            direct: Bucket::new(self.limits.direct_per_sec, now),
            presence_rate: Bucket::new(self.limits.presence_per_sec, now),
        });
        for mid in stalled { Self::remove(&mut g, &key, mid); }
        Ok((Session { room: key, id }, ServerMsg::Welcome { you: id, tick_ms: self.limits.tick_ms, members: others }))
    }

    /// The member leaves: everyone left hears Left; an empty room goes.
    pub fn leave(&self, s: &Session) {
        let mut g = self.inner.lock().unwrap();
        Self::remove(&mut g, &s.room, s.id);
    }

    /// The member's latest presence (kept for the next tick); false when over the rate (dropped).
    pub fn presence(&self, s: &Session, p: [u8; PRESENCE_SIZE]) -> bool {
        let now = (self.clock)();
        let mut g = self.inner.lock().unwrap();
        let Some(slot) = g.rooms.get_mut(&s.room).and_then(|r| r.members.get_mut(&s.id)) else { return false };
        if !slot.presence_rate.take(now) { return false; }
        slot.presence = Some(p);
        slot.presence_at = now;
        true
    }

    /// A direct (battle) message to member [to]; None when it went, else why not (also sent back as DirectRefused).
    pub fn direct(&self, s: &Session, to: u16, kind: u16, payload: Vec<u8>) -> Option<DirectRefusal> {
        let now = (self.clock)();
        let mut g = self.inner.lock().unwrap();
        let refusal = (|| {
            let r = g.rooms.get_mut(&s.room)?;
            let from_sha = {
                let me = r.members.get_mut(&s.id)?;
                if !me.direct.take(now) { return Some(Err(DirectRefusal::Rate)); }
                me.profile.rom_sha1
            };
            let Some(target) = r.members.get(&to) else { return Some(Err(DirectRefusal::NoMember)) };
            if to == s.id { return Some(Err(DirectRefusal::NoMember)); }
            if target.profile.rom_sha1 != from_sha { return Some(Err(DirectRefusal::DifferentRom)); }
            let msg = ServerMsg::Direct { from: s.id, kind, payload }.encode();
            Some(if target.tx.try_send(Out::Reliable(msg)).is_ok() { Ok(()) } else { Err(DirectRefusal::NoMember) })
        })();
        match refusal {
            None => None, // the sender itself is gone
            Some(Ok(())) => None,
            Some(Err(why)) => {
                if why == DirectRefusal::NoMember {
                    // A target whose queue is full is a stalled reader: dropped (its connection closes).
                    if let Some(t) = g.rooms.get(&s.room).and_then(|r| r.members.get(&to)) {
                        if t.tx.capacity() == 0 { Self::remove(&mut g, &s.room, to); }
                    }
                }
                if let Some(me) = g.rooms.get(&s.room).and_then(|r| r.members.get(&s.id)) {
                    let _ = me.tx.try_send(Out::Reliable(ServerMsg::DirectRefused { to, reason: why }.encode()));
                }
                Some(why)
            }
        }
    }

    /// One tick: every member gets the others' latest presence in one batch (none when no one else has any; a presence
    /// older than [PRESENCE_STALE] doesn't count). A full queue just misses this one: the next tick carries the newer
    /// state anyway.
    pub fn tick(&self) {
        let now = (self.clock)();
        let g = self.inner.lock().unwrap();
        for r in g.rooms.values() {
            let all: Vec<(u16, [u8; PRESENCE_SIZE])> = r.members.iter()
                .filter(|(_, s)| now.saturating_duration_since(s.presence_at) <= PRESENCE_STALE)
                .filter_map(|(&id, s)| s.presence.map(|p| (id, p)))
                .collect();
            if all.is_empty() { continue; }
            for (&id, s) in &r.members {
                let batch: Vec<(u16, [u8; PRESENCE_SIZE])> = all.iter().copied().filter(|(m, _)| *m != id).collect();
                if batch.is_empty() { continue; }
                let _ = s.tx.try_send(Out::Presence(ServerMsg::Presence(batch).encode()));
            }
        }
    }

    pub fn stats(&self) -> Stats {
        let g = self.inner.lock().unwrap();
        Stats { rooms: g.rooms.len(), members: g.rooms.values().map(|r| r.members.len()).sum(), clients: g.clients }
    }

    fn remove(g: &mut Inner, room: &str, id: u16) {
        let Some(r) = g.rooms.get_mut(room) else { return };
        if r.members.remove(&id).is_none() { return; }
        let left = ServerMsg::Left(id).encode();
        for s in r.members.values() { let _ = s.tx.try_send(Out::Reliable(left.clone())); }
        if r.members.is_empty() { g.rooms.remove(room); }
    }
}

impl Room {
    /// The next free member id, counting up from the last one handed out (an id isn't reused at once).
    fn free_id(&mut self) -> u16 {
        loop {
            let id = self.next_id;
            self.next_id = if self.next_id >= proto::MAX_MEMBER_ID { 1 } else { self.next_id + 1 };
            if !self.members.contains_key(&id) { return id; }
        }
    }
}

#[cfg(test)]
mod tests;
