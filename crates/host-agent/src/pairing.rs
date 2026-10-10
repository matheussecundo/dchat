//! Who may connect: one dchat tab at a time, proven by a one-time code the user reads from
//! this terminal. Pure (the clock is passed in) and unit-tested.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use protocol::{
    format_code, pair_ack, pair_proof_matches, resume_ack, resume_proof_matches, RefuseReason, PAIRING_CODE_ALPHABET,
    PAIRING_CODE_LENGTH,
};
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use rand::{Rng, RngExt};
use std::time::{Duration, Instant};

/// Wrong codes allowed before the code is burned and pairing pauses.
pub const MAX_FAILURES: u32 = 5;
const FIRST_LOCKOUT: Duration = Duration::from_secs(30);
const MAX_LOCKOUT: Duration = Duration::from_secs(600);
/// A dropped tab may come back (reload-free reconnect) within this long.
pub const RESUME_GRACE: Duration = Duration::from_secs(30);

pub fn random_code() -> String {
    (0..PAIRING_CODE_LENGTH)
        .map(|_| PAIRING_CODE_ALPHABET[UnwrapErr(SysRng).random_range(0..PAIRING_CODE_ALPHABET.len())] as char)
        .collect()
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    UnwrapErr(SysRng).fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Debug)]
struct Session {
    token: String,
    connected: bool,
    disconnected_at: Option<Instant>,
}

#[derive(Debug)]
pub struct Pairing {
    code: String,
    /// `--test-code`: never rotated (end-to-end tests only).
    fixed: bool,
    failures: u32,
    lockouts: u32,
    locked_until: Option<Instant>,
    session: Option<Session>,
}

pub enum PairOutcome {
    Paired { token: String, ack: String },
    Refused(RefuseReason),
}

impl Pairing {
    pub fn new(fixed_code: Option<String>) -> Self {
        Self {
            fixed: fixed_code.is_some(),
            code: fixed_code.unwrap_or_else(random_code),
            failures: 0,
            lockouts: 0,
            locked_until: None,
            session: None,
        }
    }

    /// The code to show the user, e.g. `K7QM-4XPA`; none while a tab is connected.
    pub fn display_code(&self) -> Option<String> {
        self.session.as_ref().map_or(true, |s| !s.connected).then(|| format_code(&self.code))
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    fn rotate(&mut self) {
        if !self.fixed {
            self.code = random_code();
        }
        self.failures = 0;
    }

    /// Pair a tab. A connected session makes us busy; a disconnected one (the tab was
    /// reloaded or closed) is replaced by whoever types the current code.
    pub fn try_pair(&mut self, agent_nonce: &str, client_nonce: &str, proof: &str, now: Instant) -> PairOutcome {
        if self.session.as_ref().is_some_and(|s| s.connected) {
            return PairOutcome::Refused(RefuseReason::Busy);
        }
        if let Some(until) = self.locked_until.filter(|until| now < *until) {
            return PairOutcome::Refused(RefuseReason::Locked { retry_secs: (until - now).as_secs().max(1) });
        }
        if pair_proof_matches(&self.code, agent_nonce, client_nonce, proof) {
            let ack = pair_ack(&self.code, client_nonce, agent_nonce);
            let token = random_token(32);
            self.session = Some(Session { token: token.clone(), connected: true, disconnected_at: None });
            // One-time: the next pairing needs a new code.
            self.rotate();
            self.lockouts = 0;
            return PairOutcome::Paired { token, ack };
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            self.rotate();
            self.lockouts += 1;
            let pause = FIRST_LOCKOUT.saturating_mul(1 << (self.lockouts - 1).min(5)).min(MAX_LOCKOUT);
            self.locked_until = Some(now + pause);
            return PairOutcome::Refused(RefuseReason::Locked { retry_secs: pause.as_secs() });
        }
        PairOutcome::Refused(RefuseReason::BadCode)
    }

    /// Reconnect the paired tab: proof made with the session token, which never travels.
    pub fn try_resume(&mut self, agent_nonce: &str, client_nonce: &str, proof: &str, now: Instant) -> Result<String, RefuseReason> {
        let Some(session) = self.session.as_mut() else {
            return Err(RefuseReason::BadToken);
        };
        let in_grace = session.connected || session.disconnected_at.is_some_and(|at| now - at < RESUME_GRACE);
        if !in_grace || !resume_proof_matches(&session.token, agent_nonce, client_nonce, proof) {
            return Err(RefuseReason::BadToken);
        }
        session.connected = true;
        session.disconnected_at = None;
        Ok(resume_ack(&session.token, client_nonce, agent_nonce))
    }

    pub fn disconnected(&mut self, now: Instant) {
        if let Some(session) = self.session.as_mut() {
            session.connected = false;
            session.disconnected_at = Some(now);
        }
    }

    /// The tab did not come back in time: the session ends. Returns whether it did.
    pub fn expire(&mut self, now: Instant) -> bool {
        let expired = self
            .session
            .as_ref()
            .is_some_and(|s| !s.connected && s.disconnected_at.is_some_and(|at| now - at >= RESUME_GRACE));
        if expired {
            self.session = None;
        }
        expired
    }

    /// `Bye`, or the user stopped control here: the session is over; a new code is shown.
    pub fn end_session(&mut self) {
        self.session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{normalize_code, pair_ack_matches, pair_proof, resume_ack_matches, resume_proof};

    #[test]
    fn test_codes_use_the_unambiguous_alphabet() {
        for _ in 0..50 {
            let code = random_code();
            assert_eq!(code.len(), 8);
            assert_eq!(normalize_code(&code), code);
        }
        assert_ne!(random_code(), random_code());
    }

    #[test]
    fn test_pairing_is_one_time_and_mutual() {
        let now = Instant::now();
        let mut pairing = Pairing::new(None);
        let code = pairing.display_code().unwrap();
        let proof = pair_proof(&code.to_lowercase(), "an", "cn");
        let PairOutcome::Paired { token, ack } = pairing.try_pair("an", "cn", &proof, now) else {
            panic!("the right code pairs");
        };
        assert!(pair_ack_matches(&code, "cn", "an", &ack), "the tab can verify the app knows the code");
        assert_eq!(pairing.display_code(), None, "no code while a session exists");
        assert!(matches!(pairing.try_pair("an2", "cn2", &pair_proof(&code, "an2", "cn2"), now), PairOutcome::Refused(RefuseReason::Busy)));

        // Resume with the token (never sent), within the grace period only.
        pairing.disconnected(now);
        assert!(pairing.display_code().is_some(), "a new code is shown while the tab is away");
        let resume = resume_proof(&token, "an3", "cn3");
        let ack = pairing.try_resume("an3", "cn3", &resume, now + Duration::from_secs(5)).unwrap();
        assert!(resume_ack_matches(&token, "cn3", "an3", &ack));
        pairing.disconnected(now);
        assert!(!pairing.expire(now + Duration::from_secs(29)));
        assert!(pairing.expire(now + RESUME_GRACE));
        assert_eq!(pairing.try_resume("a", "c", &resume_proof(&token, "a", "c"), now), Err(RefuseReason::BadToken));

        // The old code is used up.
        let next = pairing.display_code().unwrap();
        assert_ne!(next, code);
        assert!(matches!(pairing.try_pair("x", "y", &pair_proof(&code, "x", "y"), now), PairOutcome::Refused(RefuseReason::BadCode)));
    }

    #[test]
    fn test_a_new_tab_replaces_a_disconnected_session() {
        let now = Instant::now();
        let mut pairing = Pairing::new(None);
        let first = pairing.display_code().unwrap();
        let PairOutcome::Paired { token: old_token, .. } = pairing.try_pair("a", "c", &pair_proof(&first, "a", "c"), now) else {
            panic!();
        };
        pairing.disconnected(now);
        let second = pairing.display_code().unwrap();
        assert!(matches!(pairing.try_pair("a2", "c2", &pair_proof(&second, "a2", "c2"), now), PairOutcome::Paired { .. }));
        assert_eq!(
            pairing.try_resume("a3", "c3", &resume_proof(&old_token, "a3", "c3"), now),
            Err(RefuseReason::BadToken),
            "the old tab's session is gone"
        );
    }

    #[test]
    fn test_wrong_codes_burn_the_code_and_pause_pairing() {
        let now = Instant::now();
        let mut pairing = Pairing::new(None);
        let code = pairing.display_code().unwrap();
        for i in 0..MAX_FAILURES - 1 {
            let outcome = pairing.try_pair("a", "c", &pair_proof("WRONG000", "a", "c"), now);
            assert!(matches!(outcome, PairOutcome::Refused(RefuseReason::BadCode)), "attempt {i}");
        }
        let locked = pairing.try_pair("a", "c", &pair_proof("WRONG000", "a", "c"), now);
        assert!(matches!(locked, PairOutcome::Refused(RefuseReason::Locked { retry_secs: 30 })));
        // Even the right (old) code no longer works, and nothing works during the pause.
        let new_code = pairing.display_code().unwrap();
        assert_ne!(new_code, code);
        let during = pairing.try_pair("a", "c", &pair_proof(&new_code, "a", "c"), now + Duration::from_secs(10));
        assert!(matches!(during, PairOutcome::Refused(RefuseReason::Locked { .. })));
        let after = pairing.try_pair("a", "c", &pair_proof(&new_code, "a", "c"), now + Duration::from_secs(31));
        assert!(matches!(after, PairOutcome::Paired { .. }));
    }

    #[test]
    fn test_fixed_test_code_never_rotates() {
        let now = Instant::now();
        let mut pairing = Pairing::new(Some("TEST0000".into()));
        assert!(matches!(pairing.try_pair("a", "c", &pair_proof("TEST-0000", "a", "c"), now), PairOutcome::Paired { .. }));
        pairing.end_session();
        assert_eq!(pairing.display_code().as_deref(), Some("TEST-0000"));
    }
}
