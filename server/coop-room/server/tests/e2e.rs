//! The server and the app's client together, over real sockets on loopback: QUIC, TCP + TLS, the fallback between
//! them, the pin, the key, the rooms, presence ticks and direct (battle) messages.

use std::time::{Duration, Instant};

use pokedaisy_coop_client::{proto, CloseReason, Client, Config, Event, Transport, Transports};
use pokedaisy_coop_server::{config::Config as ServerConfig, Server};
use proto::{DirectRefusal, Profile, RefuseReason};

const KEY: &str = "test-key-1";

struct Running {
    rt: tokio::runtime::Runtime,
    server: Option<Server>,
    _dir: tempfile::TempDir,
}

impl Running {
    fn start(tweak: impl FnOnce(&mut ServerConfig)) -> Running {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keys"), format!("# test keys\n{KEY}\nsecond-key\n")).unwrap();
        let mut cfg = ServerConfig {
            quic_addr: "127.0.0.1:0".parse().unwrap(),
            tcp_addr: Some("127.0.0.1:0".parse().unwrap()),
            cert_path: dir.path().join("server.crt"),
            key_path: dir.path().join("server.key"),
            keys_path: dir.path().join("keys"),
            tick: Duration::from_millis(50),
            ..ServerConfig::default()
        };
        tweak(&mut cfg);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let server = rt.block_on(Server::start(cfg)).unwrap();
        Running { rt, server: Some(server), _dir: dir }
    }

    fn s(&self) -> &Server { self.server.as_ref().unwrap() }

    fn config(&self, room: &str, p: Profile, transports: Transports) -> Config {
        let s = self.s();
        let mut c = Config::new("127.0.0.1", s.quic_addr.port(), s.fingerprint, KEY, room, p);
        c.tcp_port = s.tcp_addr.map(|a| a.port()).unwrap_or(1);
        c.transports = transports;
        c.quic_timeout = Duration::from_millis(800);
        c.connect_timeout = Duration::from_secs(3);
        c
    }

    fn stop(&mut self) {
        if let Some(s) = self.server.take() { self.rt.block_on(s.stop()); }
    }
}

impl Drop for Running {
    fn drop(&mut self) { self.stop(); }
}

fn profile(tid: u16, sha: u8) -> Profile {
    Profile { name: [0xBB, 0xC2, 0xFF, 0, 0, 0, 0, 0], trainer_id: tid, secret_id: 7, female: tid % 2 == 0, rom_sha1: [sha; 20], game: "emerald".into() }
}

const WAIT: Duration = Duration::from_secs(5);

/// The next event that [want] takes (others before it are skipped), or a panic after [WAIT].
fn expect<T>(c: &Client, what: &str, mut want: impl FnMut(&Event) -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if let Some(e) = c.wait(Duration::from_millis(100)) {
            if let Some(t) = want(&e) { return t; }
            if let Event::Closed(r) = &e { panic!("{what}: closed first: {r:?}"); }
        }
    }
    panic!("{what}: never came");
}

fn welcome(c: &Client) -> (Transport, u16, Vec<u16>) {
    expect(c, "welcome", |e| match e {
        Event::Welcome { transport, you, members, .. } => Some((*transport, *you, members.iter().map(|m| m.id).collect())),
        _ => None,
    })
}

fn closed(c: &Client) -> CloseReason {
    let end = Instant::now() + WAIT + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(Event::Closed(r)) = c.wait(Duration::from_millis(100)) { return r; }
    }
    panic!("never closed");
}

/// Two players in one room see each other: joined, presence each tick, directs, left.
fn two_players(transport_a: Transports, transport_b: Transports, want_a: Transport, want_b: Transport) {
    let srv = Running::start(|_| {});
    let a = Client::connect(srv.config("Kanto Crew", profile(1, 1), transport_a));
    let (ta, ida, others) = welcome(&a);
    assert_eq!((ta, others.len()), (want_a, 0));
    let b = Client::connect(srv.config("kanto crew", profile(2, 1), transport_b));
    let (tb, idb, others) = welcome(&b);
    assert_eq!(tb, want_b);
    assert_eq!(others, vec![ida]);
    let joined = expect(&a, "joined", |e| if let Event::Joined(m) = e { Some(m.clone()) } else { None });
    assert_eq!((joined.id, joined.profile.clone()), (idb, profile(2, 1)));

    // Presence: B's latest reaches A within a few ticks (as a datagram on QUIC, a frame on TCP).
    for i in 0..5u8 { assert!(b.send_presence([i; 20])); }
    let got = expect(&a, "presence", |e| match e {
        Event::Presence(list) => list.iter().find(|(id, p)| *id == idb && p[0] == 4).map(|_| ()),
        _ => None,
    });
    let _ = got;

    // A direct both ways.
    assert!(a.send_direct(idb, 0x0201, vec![1, 2, 3]));
    let (from, kind, payload) = expect(&b, "direct", |e| match e {
        Event::Direct { from, kind, payload } => Some((*from, *kind, payload.clone())),
        _ => None,
    });
    assert_eq!((from, kind, payload), (ida, 0x0201, vec![1, 2, 3]));
    assert!(b.send_direct(ida, 0x0210, vec![9; 4096]));
    expect(&a, "big direct", |e| matches!(e, Event::Direct { payload, .. } if payload.len() == 4096).then_some(()));

    // B leaves: A hears it.
    b.close();
    assert_eq!(closed(&b), CloseReason::Closed);
    let left = expect(&a, "left", |e| if let Event::Left(id) = e { Some(*id) } else { None });
    assert_eq!(left, idb);
}

#[test]
fn two_players_over_quic() { two_players(Transports::Auto, Transports::QuicOnly, Transport::Quic, Transport::Quic); }

#[test]
fn two_players_over_tcp() { two_players(Transports::TcpOnly, Transports::TcpOnly, Transport::Tcp, Transport::Tcp); }

#[test]
fn quic_and_tcp_players_share_a_room() { two_players(Transports::QuicOnly, Transports::TcpOnly, Transport::Quic, Transport::Tcp); }

#[test]
fn auto_falls_back_to_tcp_when_quic_does_not_answer() {
    let srv = Running::start(|_| {});
    let mut cfg = srv.config("crew", profile(1, 1), Transports::Auto);
    // A UDP port nothing listens on: QUIC times out, TCP takes over.
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    cfg.quic_port = dead.local_addr().unwrap().port();
    let started = Instant::now();
    let c = Client::connect(cfg);
    let (t, _, _) = welcome(&c);
    assert_eq!(t, Transport::Tcp);
    assert!(started.elapsed() >= Duration::from_millis(700), "waited for QUIC first");
}

#[test]
fn a_server_without_tcp_still_serves_quic() {
    let srv = Running::start(|c| c.tcp_addr = None);
    let c = Client::connect(srv.config("crew", profile(1, 1), Transports::Auto));
    assert_eq!(welcome(&c).0, Transport::Quic);
}

#[test]
fn a_wrong_pin_never_connects_and_never_falls_back() {
    let srv = Running::start(|_| {});
    for t in [Transports::Auto, Transports::QuicOnly, Transports::TcpOnly] {
        let mut cfg = srv.config("crew", profile(1, 1), t);
        cfg.pin[0] ^= 0xFF;
        let c = Client::connect(cfg);
        assert_eq!(closed(&c), CloseReason::PinMismatch, "{t:?}");
    }
    assert_eq!(srv.s().hub.stats().members, 0);
}

#[test]
fn a_bad_key_is_refused() {
    let srv = Running::start(|_| {});
    for t in [Transports::QuicOnly, Transports::TcpOnly] {
        let mut cfg = srv.config("crew", profile(1, 1), t);
        cfg.key = "not-a-key".into();
        assert_eq!(closed(&Client::connect(cfg)), CloseReason::Refused(RefuseReason::BadKey), "{t:?}");
    }
    // The second key in the file works too.
    let mut cfg = srv.config("crew", profile(1, 1), Transports::TcpOnly);
    cfg.key = "second-key".into();
    welcome(&Client::connect(cfg));
}

#[test]
fn a_room_holds_one_game_family_and_its_size() {
    let srv = Running::start(|c| c.room_size = 2);
    let a = Client::connect(srv.config("crew", profile(1, 1), Transports::Auto));
    welcome(&a);
    let mut fr = srv.config("crew", profile(2, 1), Transports::Auto);
    fr.profile.game = "firered".into();
    assert_eq!(closed(&Client::connect(fr)), CloseReason::Refused(RefuseReason::WrongGame));
    let b = Client::connect(srv.config("crew", profile(3, 9), Transports::Auto)); // another Emerald-based ROM: fine
    welcome(&b);
    assert_eq!(closed(&Client::connect(srv.config("crew", profile(4, 1), Transports::Auto))), CloseReason::Refused(RefuseReason::RoomFull));
    // Another room is another room.
    welcome(&Client::connect(srv.config("other", profile(5, 1), Transports::Auto)));
}

#[test]
fn battles_only_go_between_the_same_rom() {
    let srv = Running::start(|_| {});
    let a = Client::connect(srv.config("crew", profile(1, 1), Transports::Auto));
    welcome(&a);
    let hack = Client::connect(srv.config("crew", profile(2, 9), Transports::Auto));
    let (_, hid, _) = welcome(&hack);
    assert!(a.send_direct(hid, 0x0201, vec![1]));
    let why = expect(&a, "refusal", |e| if let Event::DirectRefused { to, reason } = e { Some((*to, *reason)) } else { None });
    assert_eq!(why, (hid, DirectRefusal::DifferentRom));
    assert!(a.send_direct(0x7000, 0x0201, vec![1]));
    let why = expect(&a, "refusal", |e| if let Event::DirectRefused { reason, .. } = e { Some(*reason) } else { None });
    assert_eq!(why, DirectRefusal::NoMember);
}

#[test]
fn a_stopped_server_ends_every_connection() {
    let mut srv = Running::start(|_| {});
    let q = Client::connect(srv.config("crew", profile(1, 1), Transports::QuicOnly));
    let t = Client::connect(srv.config("crew", profile(2, 1), Transports::TcpOnly));
    welcome(&q);
    welcome(&t);
    srv.stop();
    assert!(matches!(closed(&q), CloseReason::Lost(_)));
    assert!(matches!(closed(&t), CloseReason::Lost(_) | CloseReason::Protocol(_)));
}

#[test]
fn nobody_listening_is_unreachable() {
    let dead_udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let dead_tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = dead_tcp.local_addr().unwrap().port();
    drop(dead_tcp);
    let mut cfg = Config::new("127.0.0.1", dead_udp.local_addr().unwrap().port(), [0; 32], KEY, "crew", profile(1, 1));
    cfg.tcp_port = tcp_port;
    cfg.quic_timeout = Duration::from_millis(300);
    assert!(matches!(closed(&Client::connect(cfg)), CloseReason::Unreachable(_)));
}

#[test]
fn many_players_all_see_each_other() {
    let srv = Running::start(|c| { c.room_size = 16; c.max_clients_per_ip = 64; });
    let clients: Vec<Client> = (0..12u16)
        .map(|i| Client::connect(srv.config("crowd", profile(i + 1, 1), if i % 3 == 0 { Transports::TcpOnly } else { Transports::QuicOnly })))
        .collect();
    let ids: Vec<u16> = clients.iter().map(|c| welcome(c).1).collect();
    for (c, id) in clients.iter().zip(&ids) { assert!(c.send_presence([*id as u8; 20])); }
    // Every player hears all 11 others in one tick's batch, eventually.
    for (c, id) in clients.iter().zip(&ids) {
        expect(c, "a full batch", |e| match e {
            Event::Presence(list) if list.len() == 11 && list.iter().all(|(m, p)| m != id && p[0] == *m as u8) => Some(()),
            _ => None,
        });
    }
    assert_eq!(srv.s().hub.stats().members, 12);
}

/// A server that welcomes and then says nothing more (a path a NAT dropped, a hung server): the client calls it lost
/// after three silent pings instead of looking connected for minutes.
#[test]
fn a_silent_server_is_noticed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let id = pokedaisy_coop_server::tls::make(&["pokedaisy-coop".into()]).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(pokedaisy_coop_server::tls::server_config(&id).unwrap());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let l = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = l.local_addr().unwrap().port();
    rt.spawn(async move {
        let (sock, _) = l.accept().await.unwrap();
        let mut tls = acceptor.accept(sock).await.unwrap();
        let mut buf = [0u8; 512];
        let _ = tls.read(&mut buf).await; // the Hello
        let w = proto::ServerMsg::Welcome { you: 1, tick_ms: 100, members: vec![] }.encode();
        tls.write_all(&proto::frame(&w)).await.unwrap();
        tls.flush().await.unwrap();
        // Then silence, reading (and dropping) the pings so the connection itself stays open.
        loop { if matches!(tls.read(&mut buf).await, Ok(0) | Err(_)) { break } }
    });
    let mut c = Config::new("127.0.0.1", port, id.fingerprint(), KEY, "quiet", profile(1, 1));
    c.tcp_port = port;
    c.transports = Transports::TcpOnly;
    c.ping_every = Duration::from_millis(200);
    let client = Client::connect(c);
    assert_eq!(welcome(&client).0, Transport::Tcp);
    let started = Instant::now();
    match closed(&client) {
        CloseReason::Lost(why) => assert!(why.contains("silent"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(3));
}
