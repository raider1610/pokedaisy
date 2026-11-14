use pokedaisy_coop_proto as proto;
use pokedaisy_coop_server::{config, tls, Server};

/// Log lines on stderr: `LOG_LEVEL` error / warn / info (default) / debug.
struct Logger(log::LevelFilter);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool { m.level() <= self.0 && m.target().starts_with("pokedaisy") }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) { eprintln!("{} {}", r.level(), r.args()); }
    }
    fn flush(&self) {}
}

fn main() {
    let level = match std::env::var("LOG_LEVEL").unwrap_or_default().to_ascii_lowercase().as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "debug" => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };
    log::set_boxed_logger(Box::new(Logger(level))).ok();
    log::set_max_level(level);

    let cfg = match config::from_env_and_args() {
        Ok(config::Command::Help) => { print!("{}", config::USAGE); return }
        Ok(config::Command::Fingerprint(c)) => {
            match tls::load_or_make(&c.cert_path, &c.key_path, &c.cert_names) {
                Ok(id) => { println!("{}", proto::fingerprint_text(&id.fingerprint())); return }
                Err(e) => { eprintln!("error: {e}"); std::process::exit(1) }
            }
        }
        Ok(config::Command::Run(c)) => c,
        Err(e) => { eprintln!("error: {e}\n\n{}", config::USAGE); std::process::exit(2) }
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async move {
        let server = match Server::start(cfg).await {
            Ok(s) => s,
            Err(e) => { eprintln!("error: {e}"); std::process::exit(1) }
        };
        log::info!("QUIC on udp {}", server.quic_addr);
        if let Some(a) = server.tcp_addr { log::info!("TCP + TLS on tcp {a}"); }
        log::info!("certificate fingerprint (players pin this): {}", proto::fingerprint_text(&server.fingerprint));
        let _ = tokio::signal::ctrl_c().await;
        log::info!("stopping");
        server.stop().await;
    });
}
