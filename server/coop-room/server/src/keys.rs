//! The accepted auth keys (fernbedienung's `authorized_hosts`): one per line, read again whenever the file changes, so
//! a key is added or revoked without a restart. A missing or unreadable file accepts no one.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

pub struct AuthKeys {
    path: PathBuf,
    state: Mutex<State>,
}

struct State {
    keys: Vec<String>,
    stamp: Option<(SystemTime, u64)>,
    loaded: bool,
}

impl AuthKeys {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let k = Self { path: path.into(), state: Mutex::new(State { keys: Vec::new(), stamp: None, loaded: false }) };
        k.reload_if_changed();
        k
    }

    /// How many keys are accepted now.
    pub fn count(&self) -> usize {
        self.reload_if_changed();
        self.state.lock().unwrap().keys.len()
    }

    /// Whether [key] is one of them; compared in constant time against each (a wrong key takes as long as any).
    pub fn accepts(&self, key: &str) -> bool {
        self.reload_if_changed();
        let st = self.state.lock().unwrap();
        let mut ok = false;
        for k in &st.keys { ok |= ct_eq(k.as_bytes(), key.as_bytes()); }
        ok
    }

    fn reload_if_changed(&self) {
        let stamp = stamp_of(&self.path);
        let mut st = self.state.lock().unwrap();
        // Unchanged since the last read (a file still missing counts too: it is warned about once, not per Hello).
        if st.loaded && stamp == st.stamp { return; }
        st.loaded = true;
        st.stamp = stamp;
        st.keys = match std::fs::read_to_string(&self.path) {
            Ok(text) => parse(&text),
            Err(e) => {
                log::warn!("auth keys {}: {e} (no one is accepted)", self.path.display());
                Vec::new()
            }
        };
    }
}

fn stamp_of(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The keys in a file's text: trimmed lines, blank ones and `#` comments skipped.
pub fn parse(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

/// Equal bytes, in time that depends only on the lengths.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) { d |= x ^ y; }
    d == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_and_blanks() {
        assert_eq!(parse("# keys\n\nalpha\n  beta  \n#gamma\n"), vec!["alpha", "beta"]);
    }

    #[test]
    fn reloads_when_the_file_changes_and_refuses_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("keys");
        let k = AuthKeys::new(&p);
        assert!(!k.accepts("alpha"));
        std::fs::write(&p, "alpha\n").unwrap();
        assert!(k.accepts("alpha"));
        assert!(!k.accepts("alph"));
        assert!(!k.accepts("alphaa"));
        std::fs::write(&p, "beta\nsecond\n").unwrap();   // a different length: a new stamp even within the same second
        assert!(!k.accepts("alpha"));
        assert!(k.accepts("beta"));
        assert_eq!(k.count(), 2);
        std::fs::remove_file(&p).unwrap();
        assert!(!k.accepts("beta"));
    }
}
