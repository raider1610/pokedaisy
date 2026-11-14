//! The PokeDaisy co-op room server: players join a room by name with an accepted auth key and see everyone in it. One
//! connection per player: QUIC (UDP) or, where UDP doesn't get through, TCP + TLS, both behind the server's own
//! certificate, which the app pins by its SHA-256. The server keeps each member's latest presence and sends every
//! member the others' every tick, and routes direct (battle) messages between two members playing the same ROM.

pub mod config;
pub mod conn;
pub mod hub;
pub mod keys;
pub mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::conn::{Shared, Sink};
use crate::hub::Hub;
use crate::keys::AuthKeys;

/// A running server: its real addresses (port 0 binds a free one), the pin to give players, and its tasks.
pub struct Server {
    pub quic_addr: SocketAddr,
    pub tcp_addr: Option<SocketAddr>,
    pub fingerprint: [u8; 32],
    pub hub: Arc<Hub>,
    endpoint: quinn::Endpoint,
    tasks: Vec<JoinHandle<()>>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Server {
    /// Binds both transports and starts serving. Call inside a tokio runtime.
    pub async fn start(cfg: Config) -> Result<Server, String> {
        let id = tls::load_or_make(&cfg.cert_path, &cfg.key_path, &cfg.cert_names)?;
        let fingerprint = id.fingerprint();
        let rustls_cfg = tls::server_config(&id)?;
        let hub = Hub::new((&cfg).into());
        let keys = Arc::new(AuthKeys::new(&cfg.keys_path));
        if keys.count() == 0 { log::warn!("{}: no auth keys, so no one can join", cfg.keys_path.display()); }
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared { hub: Arc::clone(&hub), stopping, keys, hello_timeout: cfg.hello_timeout, idle_timeout: cfg.idle_timeout });

        // QUIC.
        let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from((*rustls_cfg).clone()).map_err(|e| e.to_string())?;
        let mut sc = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
        let mut tr = quinn::TransportConfig::default();
        tr.max_idle_timeout(Some(cfg.idle_timeout.try_into().map_err(|_| "idle timeout too long".to_string())?));
        tr.max_concurrent_bidi_streams(1u32.into());
        tr.max_concurrent_uni_streams(0u32.into());
        tr.datagram_receive_buffer_size(Some(64 * 1024));
        sc.transport_config(Arc::new(tr));
        let endpoint = quinn::Endpoint::server(sc, cfg.quic_addr).map_err(|e| format!("QUIC on {}: {e}", cfg.quic_addr))?;
        let quic_addr = endpoint.local_addr().map_err(|e| e.to_string())?;
        let mut tasks = vec![tokio::spawn(accept_quic(endpoint.clone(), Arc::clone(&shared)))];

        // TCP + TLS.
        let tcp_addr = match cfg.tcp_addr {
            None => None,
            Some(addr) => {
                let l = TcpListener::bind(addr).await.map_err(|e| format!("TCP on {addr}: {e}"))?;
                let a = l.local_addr().map_err(|e| e.to_string())?;
                tasks.push(tokio::spawn(accept_tcp(l, tokio_rustls::TlsAcceptor::from(Arc::clone(&rustls_cfg)), Arc::clone(&shared))));
                Some(a)
            }
        };

        // The tick, and a stats line a minute.
        let h = Arc::clone(&hub);
        let period = cfg.tick;
        tasks.push(tokio::spawn(async move {
            let mut t = tokio::time::interval(period);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop { t.tick().await; h.tick(); }
        }));
        let h = Arc::clone(&hub);
        tasks.push(tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(60));
            t.tick().await;
            loop { t.tick().await; let s = h.stats(); log::info!("rooms={} members={} clients={}", s.rooms, s.members, s.clients); }
        }));
        Ok(Server { quic_addr, tcp_addr, fingerprint, hub, endpoint, tasks, stop })
    }

    /// Closes every connection and stops listening.
    pub async fn stop(self) {
        for t in &self.tasks { t.abort(); }
        // Every connection ends its loop (TCP ones shut their TLS down), then QUIC closes what is left.
        let _ = self.stop.send(true);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while self.hub.stats().clients > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.endpoint.close(0u32.into(), b"server stopping");
        let _ = tokio::time::timeout(Duration::from_secs(2), self.endpoint.wait_idle()).await;
    }
}

async fn accept_quic(endpoint: quinn::Endpoint, shared: Arc<Shared>) {
    while let Some(incoming) = endpoint.accept().await {
        // Only an address that answered a Retry counts against the limits: a forged source address can't use up
        // anyone's admissions (one round trip more per connection).
        if !incoming.remote_address_validated() {
            let _ = incoming.retry();
            continue;
        }
        let ip = incoming.remote_address().ip();
        let Some(admission) = shared.hub.admit(ip) else { incoming.refuse(); continue };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let conn = match incoming.await { Ok(c) => c, Err(e) => { log::debug!("QUIC handshake from {ip}: {e}"); return } };
            // The client opens its one stream right away (with its Hello on it).
            let (send, recv) = match tokio::time::timeout(shared.hello_timeout, conn.accept_bi()).await {
                Ok(Ok(s)) => s,
                _ => { conn.close(1u32.into(), b"no stream"); return }
            };
            let (tx, rx) = mpsc::channel(256);
            let reader = tokio::spawn(conn::read_frames(recv, tx.clone()));
            let dgrams = tokio::spawn(conn::read_datagrams(conn.clone(), tx));
            let why = conn::serve(&shared, admission, rx, Sink::Quic { conn: conn.clone(), send }).await;
            log::debug!("QUIC {ip}: {why}");
            reader.abort();
            dgrams.abort();
            // Give the last frames (a Refused) a moment to leave before the close.
            tokio::time::sleep(Duration::from_millis(50)).await;
            conn.close(0u32.into(), why.as_bytes());
        });
    }
}

async fn accept_tcp(l: TcpListener, acceptor: tokio_rustls::TlsAcceptor, shared: Arc<Shared>) {
    loop {
        let (sock, peer) = match l.accept().await {
            Ok(s) => s,
            Err(e) => {
                // Out of file descriptors and the like: back off a moment instead of spinning on the error.
                log::warn!("TCP accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Some(admission) = shared.hub.admit(peer.ip()) else { drop(sock); continue };
        let _ = sock.set_nodelay(true);
        let shared = Arc::clone(&shared);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let tls = match tokio::time::timeout(shared.hello_timeout, acceptor.accept(sock)).await {
                Ok(Ok(t)) => t,
                _ => { log::debug!("TLS handshake from {peer} failed"); return }
            };
            let (recv, send) = tokio::io::split(tls);
            let (tx, rx) = mpsc::channel(256);
            let reader = tokio::spawn(conn::read_frames(recv, tx));
            let why = conn::serve(&shared, admission, rx, Sink::Tcp { send }).await;
            log::debug!("TCP {peer}: {why}");
            reader.abort();
        });
    }
}
