//! The server's settings: every one has a command-line flag and an environment variable (the flag wins).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    /// QUIC (UDP) listen address.
    pub quic_addr: SocketAddr,
    /// TCP + TLS listen address (the fallback); None = off.
    pub tcp_addr: Option<SocketAddr>,
    /// PEM certificate and key; made (self-signed) on first start when both are missing.
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// Names the made certificate carries (the app pins the certificate, so these are informative).
    pub cert_names: Vec<String>,
    /// One accepted auth key per line (`#` comments and blank lines ignored), read again when it changes.
    pub keys_path: PathBuf,
    pub max_rooms: usize,
    pub room_size: usize,
    pub max_clients: usize,
    pub max_clients_per_ip: usize,
    /// How often every member gets the others' presence.
    pub tick: Duration,
    /// A connection must say Hello within this.
    pub hello_timeout: Duration,
    /// A connection silent this long is closed (the app pings every few seconds).
    pub idle_timeout: Duration,
    /// Direct (battle) messages per second per member, and the burst.
    pub direct_per_sec: f64,
    /// Presence updates per second per member, and the burst (the app sends 10).
    pub presence_per_sec: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quic_addr: "0.0.0.0:9595".parse().unwrap(),
            tcp_addr: Some("0.0.0.0:9595".parse().unwrap()),
            cert_path: "server.crt".into(),
            key_path: "server.key".into(),
            cert_names: vec!["pokedaisy-coop".into()],
            keys_path: "authorized_keys".into(),
            max_rooms: 1_000,
            room_size: 16,
            max_clients: 4_000,
            max_clients_per_ip: 8,
            tick: Duration::from_millis(100),
            hello_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            direct_per_sec: 400.0,
            presence_per_sec: 40.0,
        }
    }
}

pub const USAGE: &str = "\
pokedaisy-coop-server: PokeDaisy co-op rooms over QUIC (UDP) with a TCP + TLS fallback.

Flags (environment variable in brackets; the flag wins):
  --quic ADDR            QUIC listen address, UDP          [QUIC_ADDR]   default 0.0.0.0:9595
  --tcp ADDR|off         TCP + TLS fallback listen address [TCP_ADDR]    default 0.0.0.0:9595
  --cert PATH            PEM certificate                   [CERT]        default server.crt
  --key PATH             PEM private key                   [KEY]         default server.key
  --cert-name NAME       a name the made certificate holds [CERT_NAMES]  default pokedaisy-coop (comma separated in the env)
  --keys PATH            accepted auth keys, one per line  [AUTH_KEYS]   default authorized_keys
  --max-rooms N          [MAX_ROOMS]           default 1000
  --room-size N          [ROOM_SIZE]           default 16 (2..=256)
  --max-clients N        [MAX_CLIENTS]         default 4000
  --max-clients-per-ip N [MAX_CLIENTS_PER_IP]  default 8
  --tick-ms N            [TICK_MS]             default 100 (20..=1000)
  --fingerprint          print the certificate's SHA-256 fingerprint (made first if missing) and exit
  --help

The certificate and key are made, self-signed, when both are missing. Players pin its fingerprint in the app
(Settings > CO-OP > ROOM SERVER > FINGERPRINT); the server prints it at every start.
";

/// What the command line asked for.
pub enum Command {
    Run(Config),
    Fingerprint(Config),
    Help,
}

/// The settings from `args` (without the program name) and `env`; an error names the bad one.
pub fn parse(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<Command, String> {
    let mut c = Config::default();
    let mut fingerprint = false;
    // The environment first; flags override it.
    let set = |c: &mut Config, name: &str, v: &str| -> Result<(), String> {
        let num = |v: &str| v.trim().parse::<usize>().map_err(|_| format!("{name}: not a number: {v}"));
        match name {
            "quic" => c.quic_addr = v.parse().map_err(|_| format!("{name}: not an address: {v}"))?,
            "tcp" => c.tcp_addr = if v.eq_ignore_ascii_case("off") { None } else {
                Some(v.parse().map_err(|_| format!("{name}: not an address: {v}"))?)
            },
            "cert" => c.cert_path = v.into(),
            "key" => c.key_path = v.into(),
            "cert-name" => c.cert_names = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "keys" => c.keys_path = v.into(),
            "max-rooms" => c.max_rooms = num(v)?,
            "room-size" => {
                let n = num(v)?;
                if !(2..=256).contains(&n) { return Err(format!("{name}: 2..=256, got {n}")); }
                c.room_size = n;
            }
            "max-clients" => c.max_clients = num(v)?,
            "max-clients-per-ip" => c.max_clients_per_ip = num(v)?,
            "tick-ms" => {
                let n = num(v)?;
                if !(20..=1000).contains(&n) { return Err(format!("{name}: 20..=1000, got {n}")); }
                c.tick = Duration::from_millis(n as u64);
            }
            _ => return Err(format!("unknown setting {name}")),
        }
        Ok(())
    };
    for (var, name) in [
        ("QUIC_ADDR", "quic"), ("TCP_ADDR", "tcp"), ("CERT", "cert"), ("KEY", "key"), ("CERT_NAMES", "cert-name"),
        ("AUTH_KEYS", "keys"), ("MAX_ROOMS", "max-rooms"), ("ROOM_SIZE", "room-size"), ("MAX_CLIENTS", "max-clients"),
        ("MAX_CLIENTS_PER_IP", "max-clients-per-ip"), ("TICK_MS", "tick-ms"),
    ] {
        if let Some(v) = env(var) { set(&mut c, name, &v)?; }
    }
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--help" | "-h" => return Ok(Command::Help),
            "--fingerprint" => fingerprint = true,
            _ if a.starts_with("--") => {
                let name = &a[2..];
                let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
                set(&mut c, name, v)?;
                i += 1;
            }
            _ => return Err(format!("unexpected argument {a}")),
        }
        i += 1;
    }
    if c.cert_names.is_empty() { return Err("cert-name: at least one".into()); }
    Ok(if fingerprint { Command::Fingerprint(c) } else { Command::Run(c) })
}

pub fn from_env_and_args() -> Result<Command, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    parse(&args, &|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], env: &[(&str, &str)]) -> Result<Config, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let env: std::collections::HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        match parse(&args, &|k| env.get(k).cloned())? {
            Command::Run(c) => Ok(c),
            _ => Err("not run".into()),
        }
    }

    #[test]
    fn defaults_env_and_flags_with_flags_winning() {
        let c = run(&[], &[]).unwrap();
        assert_eq!(c.quic_addr.port(), 9595);
        assert_eq!(c.tcp_addr.unwrap().port(), 9595);
        let c = run(&["--room-size", "4", "--tcp", "off"], &[("ROOM_SIZE", "8"), ("TICK_MS", "50"), ("CERT_NAMES", "a.example, b")]).unwrap();
        assert_eq!(c.room_size, 4);
        assert!(c.tcp_addr.is_none());
        assert_eq!(c.tick, Duration::from_millis(50));
        assert_eq!(c.cert_names, vec!["a.example", "b"]);
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(run(&["--room-size", "1"], &[]).is_err());
        assert!(run(&["--tick-ms", "5"], &[]).is_err());
        assert!(run(&["--quic", "nowhere"], &[]).is_err());
        assert!(run(&["--room-size"], &[]).is_err());
        assert!(run(&["--bogus", "1"], &[]).is_err());
        assert!(run(&["stray"], &[]).is_err());
        assert!(run(&[], &[("MAX_ROOMS", "lots")]).is_err());
    }

    #[test]
    fn help_and_fingerprint_commands() {
        assert!(matches!(parse(&["--help".into()], &|_| None), Ok(Command::Help)));
        assert!(matches!(parse(&["--fingerprint".into()], &|_| None), Ok(Command::Fingerprint(_))));
    }
}
