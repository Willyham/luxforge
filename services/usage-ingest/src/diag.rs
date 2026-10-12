//! The service's only diagnostics: fixed status-class counters and a sink for one aggregate line.
//! Nothing request-derived can be logged because no function here takes a string from a request,
//! and the line is made of this module's own words and counts.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        /// Aggregate counters, one per outcome class.
        #[derive(Default)]
        pub struct Counters { $(pub $name: AtomicU64,)* }

        impl Counters {
            /// `name=count` pairs in a fixed order.
            pub fn line(&self) -> String {
                let mut line = String::from("usage-ingest stats");
                $(line.push_str(&format!(" {}={}", stringify!($name), self.$name.load(Relaxed)));)*
                line
            }
        }
    };
}

counters!(
    accepted,
    bad_request,
    payload_too_large,
    unsupported_media_type,
    method_not_allowed,
    not_found,
    limited_global,
    limited_id,
    limited_queue,
    id_table_full,
    connections_refused,
    connections_timed_out,
    forwarded,
    forward_failed,
    feeds_served,
    feeds_missing,
);

impl Counters {
    pub fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Relaxed);
    }
}

/// Where aggregate lines go. The binary prints to standard error; tests capture.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Diagnostics {
    pub counters: Counters,
    sink: Sink,
}

impl Diagnostics {
    pub fn new(sink: Sink) -> Diagnostics {
        Diagnostics {
            counters: Counters::default(),
            sink,
        }
    }

    /// Write one fixed message (a string literal at the call site).
    pub fn note(&self, message: &'static str) {
        (self.sink)(message);
    }

    /// Write the aggregate counters.
    pub fn emit(&self) {
        (self.sink)(&self.counters.line());
    }
}
