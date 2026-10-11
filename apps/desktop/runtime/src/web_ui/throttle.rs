//! Per-client rate limit on unauthenticated web UI requests: password sign-ins
//! and rejected API calls. After a burst, each client gets one password guess
//! per refill period; signed-in sessions never draw from it. Each client
//! address has its own budget, so one LAN client cannot lock others out of
//! signing in.

use std::{
    net::{IpAddr, Ipv6Addr},
    num::NonZeroU32,
    time::Duration,
};

use governor::{
    clock::Clock, middleware::NoOpMiddleware, state::keyed::DashMapStateStore, Quota, RateLimiter,
};

/// Unauthenticated requests allowed in a burst, then one per `THROTTLE_REFILL`.
pub const THROTTLE_BURST: NonZeroU32 = NonZeroU32::new(10).unwrap();
pub const THROTTLE_REFILL: Duration = Duration::from_secs(3);
/// Client budgets kept before idle ones (back at a full burst) are dropped.
const TRACKED_CLIENTS: usize = 1024;

/// Budgets refill by the system clock; in unit tests time stands still unless
/// a test advances it, so slow test machines cannot refill a budget early.
#[cfg(not(test))]
type ThrottleClock = governor::clock::DefaultClock;
#[cfg(test)]
type ThrottleClock = governor::clock::FakeRelativeClock;

pub struct Throttle<C: Clock = ThrottleClock>(
    RateLimiter<IpAddr, DashMapStateStore<IpAddr>, C, NoOpMiddleware<C::Instant>>,
);

impl Default for Throttle {
    fn default() -> Self {
        Self::with_clock(ThrottleClock::default())
    }
}

impl<C: Clock> Throttle<C> {
    fn with_clock(clock: C) -> Self {
        let quota = Quota::with_period(THROTTLE_REFILL)
            .expect("the refill period is not zero")
            .allow_burst(THROTTLE_BURST);
        Self(RateLimiter::new(quota, DashMapStateStore::default(), clock))
    }

    /// Takes one request from the client's budget, or returns false when the
    /// client must back off.
    pub fn allow(&self, client: IpAddr) -> bool {
        if self.0.len() >= TRACKED_CLIENTS {
            self.0.retain_recent();
        }
        self.0.check_key(&client_key(client)).is_ok()
    }
}

/// One budget per IPv4 address and per IPv6 /64, the smallest block a single
/// IPv6 host is normally assigned.
fn client_key(client: IpAddr) -> IpAddr {
    match client {
        IpAddr::V4(ip) => IpAddr::V4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => IpAddr::V4(ip),
            None => IpAddr::V6(Ipv6Addr::from_bits(ip.to_bits() & !u128::from(u64::MAX))),
        },
    }
}

#[cfg(test)]
mod tests {
    use governor::clock::FakeRelativeClock;

    use super::*;

    #[test]
    fn each_client_gets_a_burst_that_refills_slowly() {
        let clock = FakeRelativeClock::default();
        let throttle = Throttle::with_clock(clock.clone());
        let client: IpAddr = "192.168.1.30".parse().unwrap();
        for _ in 0..THROTTLE_BURST.get() {
            assert!(throttle.allow(client));
        }
        assert!(!throttle.allow(client));
        // Another client is unaffected.
        assert!(throttle.allow("192.168.1.31".parse().unwrap()));
        clock.advance(THROTTLE_REFILL / 2);
        assert!(!throttle.allow(client));
        clock.advance(THROTTLE_REFILL / 2);
        assert!(throttle.allow(client));
        assert!(!throttle.allow(client));
        clock.advance(THROTTLE_REFILL * 100);
        for _ in 0..THROTTLE_BURST.get() {
            assert!(throttle.allow(client));
        }
        assert!(!throttle.allow(client));
    }

    #[test]
    fn addresses_of_one_host_share_a_budget() {
        let throttle = Throttle::with_clock(FakeRelativeClock::default());
        for index in 0..THROTTLE_BURST.get() {
            assert!(throttle.allow(format!("fd00::{index:x}").parse().unwrap()));
        }
        assert!(!throttle.allow("fd00::ffff".parse().unwrap()));
        assert!(throttle.allow("fd00:0:0:1::1".parse().unwrap()));
        for _ in 0..THROTTLE_BURST.get() {
            assert!(throttle.allow("192.168.1.30".parse().unwrap()));
        }
        assert!(!throttle.allow("::ffff:192.168.1.30".parse().unwrap()));
    }
}
