use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn limits() -> Limits {
    Limits { max_rooms: 3, room_size: 3, max_clients: 5, max_clients_per_ip: 2, direct_per_sec: 5.0, presence_per_sec: 4.0, tick_ms: 100 }
}

fn profile(tid: u16, game: &str, sha: u8) -> Profile {
    Profile { name: [0xBB, 0xFF, 0, 0, 0, 0, 0, 0], trainer_id: tid, secret_id: 1, female: false, rom_sha1: [sha; 20], game: game.into() }
}

struct Peer { s: Session, rx: mpsc::Receiver<Out> }

impl Peer {
    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            let b = match m { Out::Reliable(b) | Out::Presence(b) => b };
            out.push(ServerMsg::decode(&b).unwrap());
        }
        out
    }
}

fn join(h: &Hub, room: &str, p: Profile) -> Result<(Peer, ServerMsg), RefuseReason> {
    let (tx, rx) = mpsc::channel(OUT_QUEUE);
    h.join(room, p, tx).map(|(s, w)| (Peer { s, rx }, w))
}

/// A clock the test moves.
fn clocked() -> (Arc<Hub>, Arc<AtomicU64>) {
    let base = Instant::now();
    let ms = Arc::new(AtomicU64::new(0));
    let m = Arc::clone(&ms);
    (Hub::with_clock(limits(), move || base + Duration::from_millis(m.load(Ordering::SeqCst))), ms)
}

#[test]
fn joining_welcomes_with_everyone_and_tells_them() {
    let h = Hub::new(limits());
    let (mut a, wa) = join(&h, "Crew", profile(1, "emerald", 1)).unwrap();
    assert_eq!(wa, ServerMsg::Welcome { you: 1, tick_ms: 100, members: vec![] });
    let (mut b, wb) = join(&h, "crew", profile(2, "emerald", 1)).unwrap(); // same room: case doesn't matter
    let ServerMsg::Welcome { you, members, .. } = wb else { panic!() };
    assert_eq!(you, 2);
    assert_eq!(members, vec![Member { id: 1, profile: profile(1, "emerald", 1) }]);
    assert_eq!(a.drain(), vec![ServerMsg::Joined(Member { id: 2, profile: profile(2, "emerald", 1) })]);
    assert!(b.drain().is_empty());
}

#[test]
fn leaving_tells_the_rest_and_an_empty_room_goes() {
    let h = Hub::new(limits());
    let (mut a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    a.drain();
    h.leave(&b.s);
    assert_eq!(a.drain(), vec![ServerMsg::Left(2)]);
    h.leave(&a.s);
    assert_eq!(h.stats().rooms, 0);
    h.leave(&a.s); // twice: nothing
}

#[test]
fn ids_are_not_reused_at_once() {
    let h = Hub::new(limits());
    let (a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    h.leave(&b.s);
    let (c, _) = join(&h, "crew", profile(3, "emerald", 1)).unwrap();
    assert_eq!((a.s.id, b.s.id, c.s.id), (1, 2, 3));
}

#[test]
fn rooms_take_one_game_family_and_a_size_limit() {
    let h = Hub::new(limits());
    let _a = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    assert_eq!(join(&h, "crew", profile(2, "firered", 2)).err(), Some(RefuseReason::WrongGame));
    // Another Emerald-based ROM is fine (presence); battles are checked per message.
    let _b = join(&h, "crew", profile(3, "emerald", 9)).unwrap();
    let _c = join(&h, "crew", profile(4, "emerald", 1)).unwrap();
    assert_eq!(join(&h, "crew", profile(5, "emerald", 1)).err(), Some(RefuseReason::RoomFull));
    assert_eq!(join(&h, "bad/name", profile(6, "emerald", 1)).err(), Some(RefuseReason::BadRoom));
}

#[test]
fn the_server_has_a_room_limit() {
    let h = Hub::new(limits());
    let _r: Vec<_> = (0..3).map(|i| join(&h, &format!("r{i}"), profile(1, "emerald", 1)).unwrap()).collect();
    assert_eq!(join(&h, "r9", profile(1, "emerald", 1)).err(), Some(RefuseReason::ServerFull));
    assert!(join(&h, "r1", profile(2, "emerald", 1)).is_ok()); // an existing room still takes people
}

#[test]
fn admissions_count_per_server_and_per_address() {
    let h = Hub::new(limits());
    let ip1: IpAddr = "10.0.0.1".parse().unwrap();
    let ip2: IpAddr = "10.0.0.2".parse().unwrap();
    let a1 = h.admit(ip1).unwrap();
    let _a2 = h.admit(ip1).unwrap();
    assert!(h.admit(ip1).is_none());
    drop(a1);
    let _a3 = h.admit(ip1).unwrap();
    let _b: Vec<_> = (0..3).map(|_| h.admit(ip2)).collect();
    assert_eq!(h.stats().clients, 4); // the third from ip2 was refused
    let _c = h.admit("10.0.0.4".parse().unwrap()).unwrap();
    assert_eq!(h.stats().clients, 5);
    assert!(h.admit("10.0.0.3".parse().unwrap()).is_none()); // the server is full
}

#[test]
fn a_tick_sends_everyone_the_others_latest_presence() {
    let (h, ms) = clocked();
    let (mut a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (mut b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    let (mut c, _) = join(&h, "crew", profile(3, "emerald", 1)).unwrap();
    for p in [&mut a, &mut b, &mut c] { p.drain(); }
    h.tick();
    assert!(a.drain().is_empty()); // no presence yet: nothing sent
    assert!(h.presence(&a.s, [1; 20]));
    ms.fetch_add(300, Ordering::SeqCst);
    assert!(h.presence(&b.s, [2; 20]));
    assert!(h.presence(&b.s, [22; 20])); // the newest wins
    h.tick();
    assert_eq!(a.drain(), vec![ServerMsg::Presence(vec![(2, [22; 20])])]);
    assert_eq!(b.drain(), vec![ServerMsg::Presence(vec![(1, [1; 20])])]);
    assert_eq!(c.drain(), vec![ServerMsg::Presence(vec![(1, [1; 20]), (2, [22; 20])])]);
    // Kept: the next tick says it again (a lost datagram is covered).
    h.tick();
    assert_eq!(a.drain().len(), 1);
}

#[test]
fn a_presence_that_stopped_coming_is_left_out() {
    let (h, ms) = clocked();
    let (mut a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (mut b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    for p in [&mut a, &mut b] { p.drain(); }
    assert!(h.presence(&a.s, [1; 20]));
    ms.fetch_add(PRESENCE_STALE.as_millis() as u64, Ordering::SeqCst);
    h.tick();
    assert_eq!(b.drain(), vec![ServerMsg::Presence(vec![(1, [1; 20])])]); // just in time
    ms.fetch_add(1, Ordering::SeqCst);
    h.tick();
    assert!(b.drain().is_empty()); // a paused player: their hero goes on the others' screens
    assert!(h.presence(&a.s, [3; 20]));
    h.tick();
    assert_eq!(b.drain(), vec![ServerMsg::Presence(vec![(1, [3; 20])])]); // and comes back with the next update
}

#[test]
fn presence_has_a_rate() {
    let (h, ms) = clocked();
    let (a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    for _ in 0..4 { assert!(h.presence(&a.s, [1; 20])); }
    assert!(!h.presence(&a.s, [1; 20]));
    ms.fetch_add(250, Ordering::SeqCst);
    assert!(h.presence(&a.s, [1; 20]));
}

#[test]
fn directs_go_to_one_member_with_the_same_rom() {
    let h = Hub::new(limits());
    let (mut a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (mut b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    let (mut hack, _) = join(&h, "crew", profile(3, "emerald", 9)).unwrap();
    for p in [&mut a, &mut b, &mut hack] { p.drain(); }
    assert_eq!(h.direct(&a.s, 2, 0x0201, vec![5]), None);
    assert_eq!(b.drain(), vec![ServerMsg::Direct { from: 1, kind: 0x0201, payload: vec![5] }]);
    assert!(hack.drain().is_empty());
    assert_eq!(h.direct(&a.s, 3, 0x0201, vec![5]), Some(DirectRefusal::DifferentRom));
    assert_eq!(a.drain(), vec![ServerMsg::DirectRefused { to: 3, reason: DirectRefusal::DifferentRom }]);
    assert!(hack.drain().is_empty());
    assert_eq!(h.direct(&a.s, 77, 0x0201, vec![]), Some(DirectRefusal::NoMember));
    assert_eq!(h.direct(&a.s, 1, 0x0201, vec![]), Some(DirectRefusal::NoMember)); // not to oneself
}

#[test]
fn directs_have_a_rate() {
    let (h, ms) = clocked();
    let (a, _) = join(&h, "crew", profile(1, "emerald", 1)).unwrap();
    let (_b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap();
    for _ in 0..5 { assert_eq!(h.direct(&a.s, 2, 1, vec![]), None); }
    assert_eq!(h.direct(&a.s, 2, 1, vec![]), Some(DirectRefusal::Rate));
    ms.fetch_add(1_000, Ordering::SeqCst);
    assert_eq!(h.direct(&a.s, 2, 1, vec![]), None);
}

#[test]
fn rooms_are_separate() {
    let h = Hub::new(limits());
    let (mut a, _) = join(&h, "one", profile(1, "emerald", 1)).unwrap();
    let (b, _) = join(&h, "two", profile(2, "firered", 2)).unwrap(); // another family in another room: fine
    assert!(h.presence(&b.s, [1; 20]));
    h.tick();
    assert!(a.drain().is_empty());
    assert_eq!(h.direct(&a.s, b.s.id, 1, vec![]), Some(DirectRefusal::NoMember)); // id 1 of room two is not in room one... nor is 1
}

#[test]
fn a_stalled_reader_is_dropped_when_a_join_cannot_reach_it() {
    let h = Hub::new(limits());
    let (tx, rx) = mpsc::channel(1);
    let (stalled, _) = h.join("crew", profile(1, "emerald", 1), tx).unwrap();
    let (mut b, _) = join(&h, "crew", profile(2, "emerald", 1)).unwrap(); // fills stalled's one slot
    let (_c, _) = join(&h, "crew", profile(3, "emerald", 1)).unwrap(); // can't reach it: dropped
    assert_eq!(h.stats().members, 2);
    assert!(b.drain().contains(&ServerMsg::Left(stalled.id)));
    drop(rx);
}
