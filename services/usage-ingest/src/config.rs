//! Configuration: defaults, and the environment variables that override them.

use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// Where to listen. Default `127.0.0.1:8080`; a reverse proxy faces the network.
    pub listen: SocketAddr,
    /// The private collector's base URL (`http://host:port[/path]`); events go to `…/events` and
    /// the profile to `…/profiles`. No default.
    pub collector_url: String,
    /// The most connections held at once; also the most requests in flight (a connection carries
    /// one request). Further connections get a `429` and are closed.
    pub max_connections: usize,
    /// The longest a connection may live, from accept to the last byte of its reply.
    pub request_timeout: Duration,
    /// Global admission: requests per second and burst for the statistics route.
    pub global_per_second: u32,
    pub global_burst: u32,
    /// The optional per-installation limiter: one batch per `id_interval_secs`, up to
    /// `id_burst`; `id_interval_secs == 0` turns it off.
    pub id_interval_secs: u64,
    pub id_burst: u32,
    /// Entries expire this long after their last request (at most 15 minutes).
    pub id_ttl_secs: u64,
    pub id_max_entries: usize,
    /// The forwarding queue: batches and bytes. Full means `429`.
    pub queue_batches: usize,
    pub queue_bytes: usize,
    /// The longest one request to the collector may take.
    pub forward_timeout: Duration,
    /// How often the aggregate counters are written.
    pub stats_interval: Duration,
    /// The `Retry-After` for a refused connection or a full queue, in seconds.
    pub retry_after_secs: u64,
}

impl Config {
    pub fn new(collector_url: impl Into<String>) -> Config {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            collector_url: collector_url.into(),
            max_connections: 128,
            request_timeout: Duration::from_secs(10),
            global_per_second: 50,
            global_burst: 100,
            id_interval_secs: 20,
            id_burst: 5,
            id_ttl_secs: 15 * 60,
            id_max_entries: 100_000,
            queue_batches: 256,
            queue_bytes: 32 * 1024 * 1024,
            forward_timeout: Duration::from_secs(5),
            stats_interval: Duration::from_secs(60),
            retry_after_secs: 30,
        }
    }

    /// Read the environment (`LUXFORGE_INGEST_*`). On a bad value, names the variable and nothing
    /// else.
    pub fn from_env() -> Result<Config, String> {
        Config::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let collector = get("LUXFORGE_INGEST_COLLECTOR_URL")
            .ok_or("LUXFORGE_INGEST_COLLECTOR_URL is required")?;
        let mut config = Config::new(collector);
        fn number<T: std::str::FromStr>(
            get: &impl Fn(&str) -> Option<String>,
            name: &str,
            target: &mut T,
        ) -> Result<(), String> {
            if let Some(text) = get(name) {
                *target = text.parse().map_err(|_| format!("{name} is not valid"))?;
            }
            Ok(())
        }
        let seconds = |get: &dyn Fn(&str) -> Option<String>, name: &str, target: &mut Duration| {
            if let Some(text) = get(name) {
                let secs: u64 = text.parse().map_err(|_| format!("{name} is not valid"))?;
                *target = Duration::from_secs(secs.max(1));
            }
            Ok::<(), String>(())
        };
        number(&get, "LUXFORGE_INGEST_LISTEN", &mut config.listen)?;
        number(
            &get,
            "LUXFORGE_INGEST_MAX_CONNECTIONS",
            &mut config.max_connections,
        )?;
        seconds(
            &get,
            "LUXFORGE_INGEST_REQUEST_TIMEOUT_SECS",
            &mut config.request_timeout,
        )?;
        number(
            &get,
            "LUXFORGE_INGEST_GLOBAL_PER_SECOND",
            &mut config.global_per_second,
        )?;
        number(
            &get,
            "LUXFORGE_INGEST_GLOBAL_BURST",
            &mut config.global_burst,
        )?;
        number(
            &get,
            "LUXFORGE_INGEST_ID_INTERVAL_SECS",
            &mut config.id_interval_secs,
        )?;
        number(&get, "LUXFORGE_INGEST_ID_BURST", &mut config.id_burst)?;
        number(&get, "LUXFORGE_INGEST_ID_TTL_SECS", &mut config.id_ttl_secs)?;
        number(
            &get,
            "LUXFORGE_INGEST_ID_MAX_ENTRIES",
            &mut config.id_max_entries,
        )?;
        number(
            &get,
            "LUXFORGE_INGEST_QUEUE_BATCHES",
            &mut config.queue_batches,
        )?;
        number(&get, "LUXFORGE_INGEST_QUEUE_BYTES", &mut config.queue_bytes)?;
        seconds(
            &get,
            "LUXFORGE_INGEST_FORWARD_TIMEOUT_SECS",
            &mut config.forward_timeout,
        )?;
        seconds(
            &get,
            "LUXFORGE_INGEST_STATS_INTERVAL_SECS",
            &mut config.stats_interval,
        )?;
        number(
            &get,
            "LUXFORGE_INGEST_RETRY_AFTER_SECS",
            &mut config.retry_after_secs,
        )?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_need_only_the_collector() {
        let config = Config::from_vars(|name| {
            (name == "LUXFORGE_INGEST_COLLECTOR_URL").then(|| "http://collector:8686".to_owned())
        })
        .unwrap();
        assert_eq!(config.max_connections, 128);
        assert!(Config::from_vars(|_| None).is_err());
    }

    #[test]
    fn a_bad_value_names_the_variable_not_the_value() {
        let error = Config::from_vars(|name| match name {
            "LUXFORGE_INGEST_COLLECTOR_URL" => Some("http://c".to_owned()),
            "LUXFORGE_INGEST_QUEUE_BYTES" => Some("sentinel-value".to_owned()),
            _ => None,
        })
        .unwrap_err();
        assert_eq!(error, "LUXFORGE_INGEST_QUEUE_BYTES is not valid");
    }
}
