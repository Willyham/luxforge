//! Fixed process-level isolation for blocking operating-system DNS. A caller owns only its
//! bounded reply slot and cancellable wait; a stuck lookup owns one fixed worker/admission slot.
//! Workers are detached, never joined by request, transport or catalog-owner shutdown.
use super::{agent::Pace, connect::Resolve};
use luxforge_core::{Error, jobs::JobControl};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Instant,
};

const DNS_WORKERS: usize = 4;
const DNS_QUEUED: usize = 16;
/// Every answer is either checked in full under this bound or refused before connecting.
pub(super) const MAX_DNS_ADDRESSES: usize = 256;

pub(super) struct ResolverService {
    queue: SyncSender<Lookup>,
    retained: Arc<AtomicUsize>,
    capacity: usize,
}

/// One reservation lasts until both the caller's reply slot and the lookup/queue slot are gone.
/// This bounds completed but unconsumed answers as well as abandoned in-flight OS calls.
struct Lease(Arc<AtomicUsize>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Reply {
    result: Mutex<Option<Result<Vec<SocketAddr>, Error>>>,
    control: Arc<JobControl>,
    _lease: Arc<Lease>,
}

struct Lookup {
    resolver: Arc<dyn Resolve>,
    host: String,
    port: u16,
    deadline: Instant,
    reply: Weak<Reply>,
    _lease: Arc<Lease>,
}

impl ResolverService {
    #[cfg(test)]
    pub(super) fn retained(&self) -> usize {
        self.retained.load(Ordering::Acquire)
    }

    pub(super) fn new(workers: usize, queued: usize) -> io::Result<Arc<Self>> {
        let (queue, receiver) = mpsc::sync_channel::<Lookup>(queued);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..workers {
            let receiver = receiver.clone();
            thread::Builder::new()
                .name(format!("luxforge-dns-{index}"))
                .spawn(move || {
                    loop {
                        // Release the receiver lock before lookup: other workers resolve independently.
                        let lookup = receiver
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .recv();
                        let Ok(lookup) = lookup else { return };
                        let Some(reply) = lookup.reply.upgrade() else {
                            continue;
                        };
                        let abandoned =
                            reply.control.is_cancelled() || Instant::now() >= lookup.deadline;
                        drop(reply);
                        if abandoned {
                            continue;
                        }
                        let result = lookup
                            .resolver
                            .resolve(&lookup.host, lookup.port)
                            .map_err(|error| {
                                Error::file_access(format!(
                                    "cannot resolve {}: {}",
                                    lookup.host,
                                    error.kind()
                                ))
                            })
                            .and_then(|addresses| {
                                if addresses.len() > MAX_DNS_ADDRESSES {
                                    Err(Error::resource_limit(format!(
                                        "{} resolved to more than {MAX_DNS_ADDRESSES} addresses",
                                        lookup.host
                                    )))
                                } else {
                                    Ok(addresses)
                                }
                            });
                        // An abandoned lookup never retains an answer or its caller's JobControl.
                        if let Some(reply) = lookup.reply.upgrade() {
                            *reply
                                .result
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(result);
                            reply.control.wake_waiters();
                        }
                    }
                })?;
        }
        Ok(Arc::new(Self {
            queue,
            retained: Arc::new(AtomicUsize::new(0)),
            capacity: workers + queued,
        }))
    }

    fn lookup(
        &self,
        resolver: Arc<dyn Resolve>,
        host: &str,
        port: u16,
        pace: &Pace,
    ) -> Result<Vec<SocketAddr>, Error> {
        let timed_out = || Error::file_access(format!("resolving {host} timed out"));
        pace.control.checkpoint()?;
        let now = Instant::now();
        let deadline = now
            .checked_add(pace.idle)
            .unwrap_or(pace.deadline)
            .min(pace.deadline);
        if now >= deadline {
            return Err(timed_out());
        }
        let reply = self.enqueue(resolver, host, port, deadline, pace.control.clone())?;
        loop {
            let observed = pace.control.wait_generation();
            pace.control.checkpoint()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timed_out());
            }
            if let Some(result) = reply
                .result
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                return result;
            }
            pace.control.wait_for_change(observed, remaining);
        }
    }

    fn enqueue(
        &self,
        resolver: Arc<dyn Resolve>,
        host: &str,
        port: u16,
        deadline: Instant,
        control: Arc<JobControl>,
    ) -> Result<Arc<Reply>, Error> {
        self.retained
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.capacity).then_some(count + 1)
            })
            .map_err(|_| Error::resource_limit("the resolver service is at capacity"))?;
        let lease = Arc::new(Lease(self.retained.clone()));
        let reply = Arc::new(Reply {
            result: Mutex::new(None),
            control,
            _lease: lease.clone(),
        });
        let lookup = Lookup {
            resolver,
            host: host.to_owned(),
            port,
            deadline,
            reply: Arc::downgrade(&reply),
            _lease: lease,
        };
        self.queue.try_send(lookup).map_err(|error| match error {
            TrySendError::Full(_) => {
                Error::resource_limit("the resolver queue has reached its limit")
            }
            TrySendError::Disconnected(_) => {
                Error::file_access("the resolver service is unavailable")
            }
        })?;
        Ok(reply)
    }
}

pub(super) fn resolve(
    service: &Option<Arc<ResolverService>>,
    resolver: Arc<dyn Resolve>,
    host: &str,
    port: u16,
    pace: &Pace,
) -> Result<Vec<SocketAddr>, Error> {
    static SYSTEM: OnceLock<Result<Arc<ResolverService>, io::ErrorKind>> = OnceLock::new();
    let service = match service {
        Some(service) => service,
        None => SYSTEM
            .get_or_init(|| {
                ResolverService::new(DNS_WORKERS, DNS_QUEUED).map_err(|error| error.kind())
            })
            .as_ref()
            .map_err(|kind| {
                Error::file_access(format!("cannot start the resolver service: {kind}"))
            })?,
    };
    service.lookup(resolver, host, port, pace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_testbase::{HANG, wait_until};

    struct Ready;
    impl Resolve for Ready {
        fn resolve(&self, _: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            Ok(vec![SocketAddr::from(([9, 9, 9, 9], port))])
        }
    }

    #[test]
    fn completed_unconsumed_answers_keep_their_bounded_admission_slots() {
        let service = ResolverService::new(1, 1).unwrap();
        let submit = || {
            service.enqueue(
                Arc::new(Ready),
                "downloads.example",
                443,
                Instant::now() + HANG,
                JobControl::new(),
            )
        };
        let first = submit().unwrap();
        wait_until("the first DNS answer", || {
            first.result.lock().unwrap().is_some()
        });
        let second = submit().unwrap();
        wait_until("the second DNS answer", || {
            second.result.lock().unwrap().is_some()
        });
        assert_eq!(service.retained(), 2);
        assert!(matches!(submit(), Err(error) if error.kind.code() == "resource-limit"));
        drop(first);
        wait_until("the consumed reply slot to be freed", || {
            service.retained() == 1
        });
        let third = submit().unwrap();
        wait_until("the admitted replacement answer", || {
            third.result.lock().unwrap().is_some()
        });
        drop(second);
        drop(third);
        wait_until("all completed reply slots to be freed", || {
            service.retained() == 0
        });
    }
}
