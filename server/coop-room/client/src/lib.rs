//! The app's side of the co-op room protocol: one connection to the room server, QUIC first and TCP + TLS when QUIC
//! doesn't get through (UDP blocked, a timeout), both checked against the server certificate's pinned SHA-256 and
//! nothing else. A pin that doesn't match is never a reason to fall back: it ends the attempt.
//!
//! [Client::connect] returns at once; the connection runs on a shared tokio runtime and reports through [Client::poll]
//! (or [Client::wait]). The app reconnects by connecting again; a [Client] serves one connection.

mod pin;
#[cfg(feature = "jni")]
mod jni_api;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
pub use pokedaisy_coop_proto as proto;
use proto::{ClientMsg, Deframer, DirectRefusal, Member, Profile, RefuseReason, ServerMsg, PRESENCE_SIZE};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

pub use pin::PinnedServer;

/// Which transports to try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transports {
    /// QUIC, then TCP + TLS if QUIC can't connect.
    Auto,
    QuicOnly,
    TcpOnly,
}

/// The transport a connection ended up on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Quic,
    Tcp,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub quic_port: u16,
    pub tcp_port: u16,
    /// The server certificate's SHA-256 (proto::parse_fingerprint of what the server prints).
    pub pin: [u8; 32],
    pub key: String,
    pub room: String,
    pub profile: Profile,
    pub transports: Transports,
    /// How long QUIC gets to connect before TCP is tried.
    pub quic_timeout: Duration,
    /// How long each whole attempt (connect, TLS, Hello, Welcome) gets.
    pub connect_timeout: Duration,
    /// A Ping this often; the server closes a connection silent for 30 s.
    pub ping_every: Duration,
}

impl Config {
    pub fn new(host: &str, port: u16, pin: [u8; 32], key: &str, room: &str, profile: Profile) -> Self {
        Self {
            host: host.into(),
            quic_port: port,
            tcp_port: port,
            pin,
            key: key.into(),
            room: room.into(),
            profile,
            transports: Transports::Auto,
            quic_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(10),
            ping_every: Duration::from_secs(5),
        }
    }
}

/// Why a connection ended (or never began).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The server's certificate isn't the pinned one: someone else answered, or the server made a new certificate.
    PinMismatch,
    /// No transport could reach the server (or it didn't answer in time).
    Unreachable(String),
    /// The server said no to the Hello.
    Refused(RefuseReason),
    /// The connection broke after it was up.
    Lost(String),
    /// The server sent something the protocol doesn't allow.
    Protocol(String),
    /// [Client::close].
    Closed,
}

/// What the connection reports, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// In the room: the transport, this player's member id, the tick and who was there already.
    Welcome { transport: Transport, you: u16, tick_ms: u16, members: Vec<Member> },
    Joined(Member),
    Left(u16),
    /// The others' latest presence (one tick).
    Presence(Vec<(u16, [u8; PRESENCE_SIZE])>),
    Direct { from: u16, kind: u16, payload: Vec<u8> },
    DirectRefused { to: u16, reason: DirectRefusal },
    /// The last event of a connection.
    Closed(CloseReason),
}

impl Event {
    /// The event as bytes for the app (RoomEventCodec.kt reads them): the server's own encoding for what the server
    /// said (Joined 0x83, Left 0x84, Presence 0x86, Direct 0x87, DirectRefused 0x88); Welcome as `0xA1`, u8 transport
    /// (1 QUIC, 2 TCP), then the server's Welcome without its kind byte; Closed as `0xA2`, u8 reason (1 pin mismatch,
    /// 2 unreachable, 3 refused, 4 lost, 5 protocol, 6 closed), u8 refuse reason (0 unless refused), u16 + UTF-8 detail.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Event::Welcome { transport, you, tick_ms, members } => {
                let w = ServerMsg::Welcome { you: *you, tick_ms: *tick_ms, members: members.clone() }.encode();
                let mut out = vec![0xA1, if *transport == Transport::Quic { 1 } else { 2 }];
                out.extend_from_slice(&w[1..]);
                out
            }
            Event::Joined(m) => ServerMsg::Joined(m.clone()).encode(),
            Event::Left(id) => ServerMsg::Left(*id).encode(),
            Event::Presence(l) => ServerMsg::Presence(l.clone()).encode(),
            Event::Direct { from, kind, payload } => ServerMsg::Direct { from: *from, kind: *kind, payload: payload.clone() }.encode(),
            Event::DirectRefused { to, reason } => ServerMsg::DirectRefused { to: *to, reason: *reason }.encode(),
            Event::Closed(r) => {
                let (code, refuse, detail) = match r {
                    CloseReason::PinMismatch => (1u8, 0u8, String::new()),
                    CloseReason::Unreachable(d) => (2, 0, d.clone()),
                    CloseReason::Refused(why) => (3, *why as u8, String::new()),
                    CloseReason::Lost(d) => (4, 0, d.clone()),
                    CloseReason::Protocol(d) => (5, 0, d.clone()),
                    CloseReason::Closed => (6, 0, String::new()),
                };
                let mut d = detail.into_bytes();
                d.truncate(1024);
                let mut out = vec![0xA2, code, refuse];
                out.extend_from_slice(&(d.len() as u16).to_be_bytes());
                out.extend_from_slice(&d);
                out
            }
        }
    }
}

enum Cmd {
    Presence([u8; PRESENCE_SIZE]),
    Direct { to: u16, kind: u16, payload: Vec<u8> },
    Close,
}

/// One connection to a room server.
pub struct Client {
    cmds: mpsc::UnboundedSender<Cmd>,
    events: Arc<Events>,
}

#[derive(Default)]
struct Events {
    q: Mutex<VecDeque<Event>>,
    cv: Condvar,
}

/// Presence batches kept waiting for the app at most: past this the oldest goes (a newer one says more), so an app that
/// stops polling can't make the queue grow without end. Other events are few and always kept.
pub const MAX_WAITING_PRESENCE: usize = 64;

impl Events {
    fn push(&self, e: Event) {
        let mut q = self.q.lock().unwrap();
        if matches!(e, Event::Presence(_)) && q.iter().filter(|x| matches!(x, Event::Presence(_))).count() >= MAX_WAITING_PRESENCE {
            if let Some(i) = q.iter().position(|x| matches!(x, Event::Presence(_))) { q.remove(i); }
        }
        q.push_back(e);
        drop(q);
        self.cv.notify_all();
    }
}

/// The runtime every client shares (two worker threads: a phone's one connection needs little).
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("pokedaisy-coop-room")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

impl Client {
    /// Starts connecting; events follow through [poll] / [wait], ending with [Event::Closed].
    pub fn connect(cfg: Config) -> Client {
        let (tx, rx) = mpsc::unbounded_channel();
        let events = Arc::new(Events::default());
        let ev = Arc::clone(&events);
        runtime().spawn(async move {
            let reason = run(cfg, rx, &ev).await;
            ev.push(Event::Closed(reason));
        });
        Client { cmds: tx, events }
    }

    /// The next event, if one is waiting.
    pub fn poll(&self) -> Option<Event> { self.events.q.lock().unwrap().pop_front() }

    /// The next event, waiting up to [timeout] for one.
    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        let q = self.events.q.lock().unwrap();
        let (mut q, _) = self.events.cv.wait_timeout_while(q, timeout, |q| q.is_empty()).unwrap();
        q.pop_front()
    }

    /// The player's presence; false once the connection is gone. Sent at once (the server keeps the latest per tick).
    pub fn send_presence(&self, p: [u8; PRESENCE_SIZE]) -> bool { self.cmds.send(Cmd::Presence(p)).is_ok() }

    /// A direct (battle) message to member [to]; false once the connection is gone or the payload is too large.
    pub fn send_direct(&self, to: u16, kind: u16, payload: Vec<u8>) -> bool {
        if payload.len() > proto::MAX_DIRECT || !proto::valid_member_id(to) { return false; }
        self.cmds.send(Cmd::Direct { to, kind, payload }).is_ok()
    }

    /// Says Bye and closes; [Event::Closed] (Closed) follows.
    pub fn close(&self) { let _ = self.cmds.send(Cmd::Close); }
}

impl Drop for Client {
    fn drop(&mut self) { let _ = self.cmds.send(Cmd::Close); }
}

// ------------------------------------------------------------------------------------------------ the connection

/// Where the connection writes, and how it reads (frames from the stream, datagrams on QUIC).
enum Wire {
    Quic { conn: quinn::Connection, send: quinn::SendStream, _endpoint: quinn::Endpoint },
    Tcp { send: tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> },
}

impl Wire {
    async fn frame(&mut self, msg: &ClientMsg) -> std::io::Result<()> {
        let f = proto::frame(&msg.encode());
        match self {
            Wire::Quic { send, .. } => send.write_all(&f).await.map_err(std::io::Error::other),
            Wire::Tcp { send } => { send.write_all(&f).await?; send.flush().await }
        }
    }

    /// Presence: a datagram on QUIC (a lost one is replaced by the next), a frame on TCP.
    async fn presence(&mut self, p: [u8; PRESENCE_SIZE]) -> std::io::Result<()> {
        if let Wire::Quic { conn, .. } = self {
            if conn.max_datagram_size().is_some() {
                let _ = conn.send_datagram(Bytes::from(ClientMsg::Presence(p).encode()));
                return Ok(());
            }
        }
        self.frame(&ClientMsg::Presence(p)).await
    }

    fn transport(&self) -> Transport { match self { Wire::Quic { .. } => Transport::Quic, Wire::Tcp { .. } => Transport::Tcp } }

    async fn close(self) {
        match self {
            Wire::Quic { conn, mut send, _endpoint } => {
                let _ = send.finish();
                conn.close(0u32.into(), b"bye");
                let _ = tokio::time::timeout(Duration::from_millis(500), _endpoint.wait_idle()).await;
            }
            Wire::Tcp { mut send } => { let _ = send.shutdown().await; }
        }
    }
}

/// What the readers hand the loop: a server message, or why reading ended.
type Inbound = Result<ServerMsg, CloseReason>;

async fn run(cfg: Config, mut cmds: mpsc::UnboundedReceiver<Cmd>, events: &Events) -> CloseReason {
    let (wire, mut inbox, welcome) = match tokio::time::timeout(cfg.connect_timeout + cfg.quic_timeout, establish(&cfg)).await {
        Err(_) => return CloseReason::Unreachable("timed out".into()),
        Ok(Err(r)) => return r,
        Ok(Ok(x)) => x,
    };
    let ServerMsg::Welcome { you, tick_ms, members } = welcome else { unreachable!() };
    events.push(Event::Welcome { transport: wire.transport(), you, tick_ms, members });
    let mut wire = wire;
    let mut ping = tokio::time::interval(cfg.ping_every);
    ping.tick().await;
    let mut ping_n = 0u64;
    // A server that says nothing (not even Pong) for three pings is gone: a TCP path a NAT dropped stays "up" for
    // minutes otherwise (QUIC has its own idle timeout, this covers both).
    let silent_after = cfg.ping_every * 3;
    let mut last_heard = tokio::time::Instant::now();
    let reason = loop {
        tokio::select! {
            c = cmds.recv() => {
                let r = match c {
                    None | Some(Cmd::Close) => { let _ = wire.frame(&ClientMsg::Bye).await; break CloseReason::Closed }
                    Some(Cmd::Presence(p)) => wire.presence(p).await,
                    Some(Cmd::Direct { to, kind, payload }) => wire.frame(&ClientMsg::Direct { to, kind, payload }).await,
                };
                if let Err(e) = r { break CloseReason::Lost(e.to_string()) }
            }
            m = inbox.recv() => {
                last_heard = tokio::time::Instant::now();
                match m {
                    None => break CloseReason::Lost("connection ended".into()),
                    Some(Err(r)) => break r,
                    Some(Ok(msg)) => match msg {
                        ServerMsg::Joined(m) => events.push(Event::Joined(m)),
                        ServerMsg::Left(id) => events.push(Event::Left(id)),
                        ServerMsg::Presence(list) => events.push(Event::Presence(list)),
                        ServerMsg::Direct { from, kind, payload } => events.push(Event::Direct { from, kind, payload }),
                        ServerMsg::DirectRefused { to, reason } => events.push(Event::DirectRefused { to, reason }),
                        ServerMsg::Pong(_) => {}
                        ServerMsg::Refused(r) => break CloseReason::Refused(r),
                        ServerMsg::Welcome { .. } => break CloseReason::Protocol("a second welcome".into()),
                    },
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > silent_after { break CloseReason::Lost("the server went silent".into()) }
                ping_n += 1;
                if let Err(e) = wire.frame(&ClientMsg::Ping(ping_n)).await { break CloseReason::Lost(e.to_string()) }
            }
        }
    };
    wire.close().await;
    reason
}

/// Connects (QUIC, then TCP), says Hello and waits for the Welcome.
async fn establish(cfg: &Config) -> Result<(Wire, mpsc::Receiver<Inbound>, ServerMsg), CloseReason> {
    let pin = PinnedServer::new(cfg.pin);
    let mut why_not = String::new();
    if cfg.transports != Transports::TcpOnly {
        match tokio::time::timeout(cfg.quic_timeout, connect_quic(cfg, &pin)).await {
            Ok(Ok((wire, rx))) => return hello(cfg, wire, rx).await,
            Ok(Err(e)) => {
                if pin.mismatched() { return Err(CloseReason::PinMismatch); }
                why_not = format!("QUIC: {e}");
            }
            Err(_) => why_not = "QUIC: timed out".into(),
        }
        if pin.mismatched() { return Err(CloseReason::PinMismatch); }
        if cfg.transports == Transports::QuicOnly { return Err(CloseReason::Unreachable(why_not)); }
        log::debug!("{why_not}; trying TCP");
    }
    match connect_tcp(cfg, &pin).await {
        Ok((wire, rx)) => hello(cfg, wire, rx).await,
        Err(e) => {
            if pin.mismatched() { return Err(CloseReason::PinMismatch); }
            let tcp = format!("TCP: {e}");
            Err(CloseReason::Unreachable(if why_not.is_empty() { tcp } else { format!("{why_not}; {tcp}") }))
        }
    }
}

async fn hello(cfg: &Config, mut wire: Wire, mut rx: mpsc::Receiver<Inbound>) -> Result<(Wire, mpsc::Receiver<Inbound>, ServerMsg), CloseReason> {
    let h = ClientMsg::Hello { version: proto::PROTOCOL_VERSION, key: cfg.key.clone(), room: cfg.room.clone(), profile: cfg.profile.clone() };
    if let Err(e) = wire.frame(&h).await { return Err(CloseReason::Unreachable(format!("hello: {e}"))); }
    match rx.recv().await {
        Some(Ok(w @ ServerMsg::Welcome { .. })) => Ok((wire, rx, w)),
        Some(Ok(ServerMsg::Refused(r))) => { wire.close().await; Err(CloseReason::Refused(r)) }
        Some(Ok(other)) => Err(CloseReason::Protocol(format!("expected a welcome, got {other:?}"))),
        Some(Err(r)) => Err(r),
        None => Err(CloseReason::Unreachable("closed before the welcome".into())),
    }
}

async fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(|e| format!("{host}: {e}"))?.collect();
    // IPv4 first: a phone's IPv6 is the likelier to be half working.
    addrs.sort_by_key(|a| !a.is_ipv4());
    addrs.into_iter().next().ok_or_else(|| format!("{host}: no address"))
}

/// The name TLS sends (the pin decides; the name only has to be well formed).
fn server_name(host: &str) -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::try_from(host.to_string())
        .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("pokedaisy-coop").unwrap())
}

fn tls_config(pin: &PinnedServer) -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut c = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(pin.clone()))
        .with_no_client_auth();
    c.alpn_protocols = vec![proto::ALPN.to_vec()];
    c.enable_early_data = false;
    Ok(c)
}

async fn connect_quic(cfg: &Config, pin: &PinnedServer) -> Result<(Wire, mpsc::Receiver<Inbound>), String> {
    let addr = resolve(&cfg.host, cfg.quic_port).await?;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config(pin)?).map_err(|e| e.to_string())?;
    let mut qc = quinn::ClientConfig::new(Arc::new(crypto));
    let mut tr = quinn::TransportConfig::default();
    tr.keep_alive_interval(Some(Duration::from_secs(5)));
    tr.max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
    qc.transport_config(Arc::new(tr));
    let bind: SocketAddr = if addr.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
    let mut endpoint = quinn::Endpoint::client(bind).map_err(|e| e.to_string())?;
    endpoint.set_default_client_config(qc);
    let name = match server_name(&cfg.host) {
        rustls::pki_types::ServerName::DnsName(d) => d.as_ref().to_string(),
        _ => "pokedaisy-coop".to_string(),
    };
    let conn = endpoint.connect(addr, &name).map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    let (send, recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(read_stream(recv, tx.clone()));
    let c2 = conn.clone();
    tokio::spawn(async move {
        loop {
            match c2.read_datagram().await {
                Ok(d) => match ServerMsg::decode(&d) {
                    Ok(m @ ServerMsg::Presence(_)) => { if tx.try_send(Ok(m)).is_err() && tx.is_closed() { return } }
                    _ => {} // only presence comes as a datagram
                },
                Err(_) => return,
            }
        }
    });
    Ok((Wire::Quic { conn, send, _endpoint: endpoint }, rx))
}

async fn connect_tcp(cfg: &Config, pin: &PinnedServer) -> Result<(Wire, mpsc::Receiver<Inbound>), String> {
    let addr = resolve(&cfg.host, cfg.tcp_port).await?;
    let sock = tokio::time::timeout(cfg.connect_timeout, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let _ = sock.set_nodelay(true);
    let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config(pin)?))
        .connect(server_name(&cfg.host), sock)
        .await
        .map_err(|e| e.to_string())?;
    let (recv, send) = tokio::io::split(tls);
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(read_stream(recv, tx));
    Ok((Wire::Tcp { send }, rx))
}

async fn read_stream<R: AsyncRead + Unpin>(mut recv: R, tx: mpsc::Sender<Inbound>) {
    let mut d = Deframer::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match recv.read(&mut buf).await {
            Ok(0) => { let _ = tx.send(Err(CloseReason::Lost("the server closed the connection".into()))).await; return }
            Err(e) => { let _ = tx.send(Err(CloseReason::Lost(e.to_string()))).await; return }
            Ok(n) => n,
        };
        d.push(&buf[..n]);
        loop {
            match d.next() {
                Ok(Some(m)) => match ServerMsg::decode(&m) {
                    Ok(msg) => if tx.send(Ok(msg)).await.is_err() { return },
                    Err(e) => { let _ = tx.send(Err(CloseReason::Protocol(e.to_string()))).await; return }
                },
                Ok(None) => break,
                Err(e) => { let _ = tx.send(Err(CloseReason::Protocol(e.to_string()))).await; return }
            }
        }
    }
}
