//! One player's connection, whatever carries it: the Hello (with its deadline), then the loop that feeds the hub what
//! the player says and writes back what the hub queued. QUIC: the client's first bidirectional stream plus datagrams;
//! TCP + TLS: one stream with presence as frames too.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pokedaisy_coop_proto::{self as proto, ClientMsg, Deframer, RefuseReason, ServerMsg};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};

use crate::hub::{Admission, Hub, Out, Session, OUT_QUEUE};
use crate::keys::AuthKeys;

/// What every connection needs from the server.
pub struct Shared {
    pub hub: Arc<Hub>,
    /// Flips to true when the server stops: every connection ends.
    pub stopping: tokio::sync::watch::Receiver<bool>,
    pub keys: Arc<AuthKeys>,
    pub hello_timeout: Duration,
    pub idle_timeout: Duration,
}

/// Where the hub's output goes for one connection.
pub enum Sink<W> {
    Quic { conn: quinn::Connection, send: W },
    Tcp { send: W },
}

/// How long one write may take: a peer that stops reading must not hold its task (and its admission) forever, nor
/// keep the server from stopping.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

impl<W: AsyncWrite + Unpin> Sink<W> {
    async fn reliable(&mut self, msg: &[u8]) -> std::io::Result<()> {
        let f = proto::frame(msg);
        let write = async {
            match self {
                Sink::Quic { send, .. } | Sink::Tcp { send } => send.write_all(&f).await,
            }
        };
        match timeout(WRITE_TIMEOUT, write).await {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "write timed out")),
        }
    }

    /// A presence batch: a datagram on QUIC when the peer takes datagrams that big, else a frame like the rest.
    async fn presence(&mut self, msg: &[u8]) -> std::io::Result<()> {
        if let Sink::Quic { conn, .. } = self {
            if conn.max_datagram_size().is_some_and(|max| msg.len() <= max) {
                // A datagram that can't go now is a lost update: the next tick sends a newer one.
                let _ = conn.send_datagram(Bytes::copy_from_slice(msg));
                return Ok(());
            }
        }
        self.reliable(msg).await
    }

    async fn flush(&mut self) {
        match self {
            Sink::Quic { send, .. } | Sink::Tcp { send } => { let _ = timeout(WRITE_TIMEOUT, send.flush()).await; }
        }
    }
}

/// Reads frames off [recv] into [to] (a decode error or the end closes the connection: None).
pub async fn read_frames<R: AsyncRead + Unpin>(mut recv: R, to: mpsc::Sender<Option<ClientMsg>>) {
    let mut d = Deframer::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match recv.read(&mut buf).await { Ok(0) | Err(_) => break, Ok(n) => n };
        d.push(&buf[..n]);
        loop {
            match d.next() {
                Ok(Some(m)) => match ClientMsg::decode(&m) {
                    Ok(msg) => if to.send(Some(msg)).await.is_err() { return },
                    Err(e) => { log::debug!("bad frame: {e}"); let _ = to.send(None).await; return; }
                },
                Ok(None) => break,
                Err(e) => { log::debug!("bad frame: {e}"); let _ = to.send(None).await; return; }
            }
        }
    }
    let _ = to.send(None).await;
}

/// Reads QUIC datagrams (presence) into [to].
pub async fn read_datagrams(conn: quinn::Connection, to: mpsc::Sender<Option<ClientMsg>>) {
    while let Ok(d) = conn.read_datagram().await {
        // Only presence travels as a datagram; anything else there is dropped (a datagram can't be trusted to arrive).
        if let Ok(m @ ClientMsg::Presence(_)) = ClientMsg::decode(&d) {
            if to.try_send(Some(m)).is_err() && to.is_closed() { return; }
        }
    }
}

/// Serves one admitted connection to its end. [inbox] carries everything the player sends (None: the stream ended or
/// broke); [sink] is where the hub's output goes. Returns why it ended, for the log.
pub async fn serve<W: AsyncWrite + Unpin>(
    shared: &Shared,
    _admission: Admission,
    mut inbox: mpsc::Receiver<Option<ClientMsg>>,
    mut sink: Sink<W>,
) -> &'static str {
    let mut stopping = shared.stopping.clone();
    // --- Hello, within its deadline.
    let hello = match timeout(shared.hello_timeout, inbox.recv()).await {
        Err(_) => return "no hello",
        Ok(None) | Ok(Some(None)) => return "closed before hello",
        Ok(Some(Some(m))) => m,
    };
    let ClientMsg::Hello { version, key, room, profile } = hello else {
        refuse(&mut sink, RefuseReason::BadHello).await;
        return "first message not a hello";
    };
    if version != proto::PROTOCOL_VERSION {
        refuse(&mut sink, RefuseReason::Version).await;
        return "other version";
    }
    if !shared.keys.accepts(&key) {
        refuse(&mut sink, RefuseReason::BadKey).await;
        return "bad key";
    }
    let (tx, mut out) = mpsc::channel::<Out>(OUT_QUEUE);
    let (session, welcome) = match shared.hub.join(&room, profile, tx) {
        Ok(j) => j,
        Err(r) => { refuse(&mut sink, r).await; return "refused"; }
    };
    let guard = LeaveOnDrop { hub: &shared.hub, session: &session };
    if sink.reliable(&welcome.encode()).await.is_err() { return "write failed"; }
    sink.flush().await;

    // --- The room.
    let mut last_heard = Instant::now();
    let reason = loop {
        let idle_at = last_heard + shared.idle_timeout;
        tokio::select! {
            msg = inbox.recv() => {
                let Some(Some(msg)) = msg else { break "closed" };
                last_heard = Instant::now();
                match msg {
                    ClientMsg::Hello { .. } => break "second hello",
                    ClientMsg::Presence(p) => { shared.hub.presence(&session, p); }
                    ClientMsg::Direct { to, kind, payload } => { shared.hub.direct(&session, to, kind, payload); }
                    ClientMsg::Ping(n) => if sink.reliable(&ServerMsg::Pong(n).encode()).await.is_err() { break "write failed" },
                    ClientMsg::Bye => break "bye",
                }
            }
            o = out.recv() => {
                // None: the hub dropped this member (its queue filled: a reader that stopped).
                let Some(o) = o else { break "dropped by the hub" };
                let r = match o { Out::Reliable(m) => sink.reliable(&m).await, Out::Presence(m) => sink.presence(&m).await };
                if r.is_err() { break "write failed" }
            }
            _ = tokio::time::sleep_until(idle_at) => break "idle",
            _ = stopping.changed() => break "server stopping",
        }
    };
    drop(guard);
    sink.flush().await;
    if let Sink::Tcp { send } = &mut sink { let _ = timeout(WRITE_TIMEOUT, send.shutdown()).await; }
    reason
}

async fn refuse<W: AsyncWrite + Unpin>(sink: &mut Sink<W>, r: RefuseReason) {
    let _ = sink.reliable(&ServerMsg::Refused(r).encode()).await;
    sink.flush().await;
}

/// Leaves the room however the loop ends.
struct LeaveOnDrop<'a> { hub: &'a Arc<Hub>, session: &'a Session }

impl Drop for LeaveOnDrop<'_> {
    fn drop(&mut self) { self.hub.leave(self.session); }
}
