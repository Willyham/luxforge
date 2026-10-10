-- Luxforge usage statistics: the whole ClickHouse schema. Every statement is idempotent, so the
-- container applies the file at every start (CLICKHOUSE_ALWAYS_RUN_INITDB_SCRIPTS). A change to an
-- existing table needs an explicit ALTER; this file never drops or rewrites stored rows.
--
-- Stored: the approved per-event rows and the per-session release/device profile, all from the
-- ingress's validated NDJSON. Nothing else: no address, header, body, request identifier, photo
-- attribute or additional key. Every table expires 90 days after `received_at`.

CREATE DATABASE IF NOT EXISTS luxforge;

-- One row per event. Duplicates (a batch received twice) share (session_id, seq) and collapse to one
-- row at merge time; the views collapse them at query time too (FINAL).
CREATE TABLE IF NOT EXISTS luxforge.events
(
    received_at     DateTime('UTC'),
    installation_id UUID,
    session_id      UUID,
    session_start   DateTime('UTC'),
    batch_seq       UInt32,
    seq             UInt32,
    t               UInt32,
    name            LowCardinality(String),
    release         LowCardinality(String),
    `trigger`       LowCardinality(Nullable(String)),
    state           LowCardinality(Nullable(String)),
    workspace       LowCardinality(Nullable(String)),
    image           Nullable(String),
    feature         LowCardinality(Nullable(String)),
    tool            LowCardinality(Nullable(String)),
    count           Nullable(UInt32),
    operation       LowCardinality(Nullable(String)),
    outcome         LowCardinality(Nullable(String)),
    duration_ms     Nullable(UInt32),
    renderer        LowCardinality(Nullable(String)),
    cpu_percent     Nullable(UInt32),
    memory_mib      Nullable(UInt32),
    memory_kind     LowCardinality(Nullable(String)),
    gpu_percent     Nullable(UInt32),
    gpu_memory_mib  Nullable(UInt32),
    INDEX image_bloom image TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX installation_bloom installation_id TYPE bloom_filter(0.01) GRANULARITY 4
)
ENGINE = ReplacingMergeTree(received_at)
ORDER BY (session_id, seq)
TTL received_at + INTERVAL 90 DAY DELETE;

-- Vector's landing table for profile rows. It stores nothing (Null engine): the two views below
-- split each row into the session's profile and the batch's loss counter.
CREATE TABLE IF NOT EXISTS luxforge.profile_batches
(
    received_at           DateTime('UTC'),
    installation_id       UUID,
    session_id            UUID,
    release               LowCardinality(String),
    batch_seq             UInt32,
    dropped               UInt32,
    os_family             LowCardinality(String),
    os_version            Array(UInt32),
    os_build              Nullable(String),
    os_distro             LowCardinality(Nullable(String)),
    os_distro_version     Array(UInt32),
    os_kernel             Array(UInt32),
    arch                  LowCardinality(String),
    cpu_vendor            LowCardinality(String),
    cpu_physical_cores    UInt16,
    cpu_logical_cores     UInt16,
    cpu_generation        Nullable(UInt8),
    cpu_tier              LowCardinality(Nullable(String)),
    cpu_family            Nullable(UInt16),
    cpu_model             Nullable(UInt16),
    cpu_stepping          Nullable(UInt16),
    memory_gib_band       LowCardinality(String),
    gpu_count             UInt8,
    gpu0_vendor           LowCardinality(Nullable(String)),
    gpu0_backend          LowCardinality(Nullable(String)),
    gpu0_type             LowCardinality(Nullable(String)),
    gpu0_unified_memory   Nullable(Bool),
    gpu0_vendor_id        Nullable(UInt16),
    gpu0_device_id        Nullable(UInt16),
    gpu0_generation       Nullable(UInt8),
    gpu0_tier             LowCardinality(Nullable(String)),
    gpu0_memory_gib_band  LowCardinality(Nullable(String)),
    gpu0_driver_version   Array(UInt32),
    gpu1_vendor           LowCardinality(Nullable(String)),
    gpu1_backend          LowCardinality(Nullable(String)),
    gpu1_type             LowCardinality(Nullable(String)),
    gpu1_unified_memory   Nullable(Bool),
    gpu1_vendor_id        Nullable(UInt16),
    gpu1_device_id        Nullable(UInt16),
    gpu1_generation       Nullable(UInt8),
    gpu1_tier             LowCardinality(Nullable(String)),
    gpu1_memory_gib_band  LowCardinality(Nullable(String)),
    gpu1_driver_version   Array(UInt32),
    renderer              LowCardinality(String),
    renderer_fallback     LowCardinality(Nullable(String))
)
ENGINE = Null;

-- One profile per session, replaced when a later batch (higher batch_seq) carries a profile.
CREATE TABLE IF NOT EXISTS luxforge.profiles AS luxforge.profile_batches
ENGINE = ReplacingMergeTree(batch_seq)
ORDER BY session_id
TTL received_at + INTERVAL 90 DAY DELETE;

-- The loss counter of every batch: the only other thing kept from a profile row.
CREATE TABLE IF NOT EXISTS luxforge.batches
(
    received_at     DateTime('UTC'),
    installation_id UUID,
    session_id      UUID,
    batch_seq       UInt32,
    dropped         UInt32
)
ENGINE = ReplacingMergeTree(received_at)
ORDER BY (session_id, batch_seq)
TTL received_at + INTERVAL 90 DAY DELETE;

CREATE MATERIALIZED VIEW IF NOT EXISTS luxforge.profiles_from_batches TO luxforge.profiles
DEFINER = luxforge_admin SQL SECURITY DEFINER
AS SELECT * FROM luxforge.profile_batches;

CREATE MATERIALIZED VIEW IF NOT EXISTS luxforge.batches_from_batches TO luxforge.batches
DEFINER = luxforge_admin SQL SECURITY DEFINER
AS SELECT received_at, installation_id, session_id, batch_seq, dropped FROM luxforge.profile_batches;

-- ---------------------------------------------------------------------------------------------
-- Read side. Dashboards query only these views. Each hides rows past the 90 days even before a
-- merge removes them, and collapses duplicates on (session_id, seq) / session_id / (session_id,
-- batch_seq).
-- ---------------------------------------------------------------------------------------------

CREATE VIEW IF NOT EXISTS luxforge.events_unique AS
SELECT * FROM luxforge.events FINAL
WHERE received_at > now() - INTERVAL 90 DAY;

CREATE VIEW IF NOT EXISTS luxforge.batches_unique AS
SELECT * FROM luxforge.batches FINAL
WHERE received_at > now() - INTERVAL 90 DAY;

-- The latest profile of each session, with the cohort labels dashboards group by.
CREATE VIEW IF NOT EXISTS luxforge.session_profiles AS
SELECT
    session_id,
    installation_id,
    release,
    splitByChar('+', release)[1] AS app,
    os_family,
    arrayStringConcat(arrayMap(x -> toString(x), os_version), '.') AS os_version_text,
    concat(os_family, ' ', toString(os_version[1])) AS os,
    arch,
    cpu_vendor,
    cpu_physical_cores,
    cpu_logical_cores,
    multiIf(
        cpu_vendor = 'apple', concat('apple gen', ifNull(toString(cpu_generation), '?'), ' ', ifNull(cpu_tier, 'base')),
        cpu_vendor IN ('intel', 'amd'), concat(cpu_vendor, ' family ', ifNull(toString(cpu_family), '?'), ' model ', ifNull(toString(cpu_model), '?')),
        cpu_vendor) AS cpu,
    memory_gib_band,
    gpu_count,
    if(gpu_count = 0, 'none',
        multiIf(
            gpu0_vendor = 'apple', concat('apple gen', ifNull(toString(gpu0_generation), '?'), ' ', ifNull(gpu0_tier, 'base'), ' ', ifNull(gpu0_backend, '')),
            gpu0_vendor_id IS NOT NULL, concat(ifNull(gpu0_vendor, 'other'), ' ', ifNull(gpu0_type, ''), ' device ', toString(gpu0_device_id), ' ', ifNull(gpu0_backend, '')),
            concat(ifNull(gpu0_vendor, 'other'), ' ', ifNull(gpu0_type, ''), ' ', ifNull(gpu0_backend, '')))) AS gpu,
    renderer,
    renderer_fallback,
    received_at AS profile_received_at
FROM luxforge.profiles FINAL
WHERE received_at > now() - INTERVAL 90 DAY;

-- Events with a readable label, wall-clock time, and `missing_before`: how many events with a lower
-- `seq` are absent between the previous kept event of the same session and this one (events or whole
-- batches lost, or dropped by the desktop under its window limits).
CREATE VIEW IF NOT EXISTS luxforge.event_timeline AS
SELECT
    received_at,
    installation_id,
    session_id,
    session_start,
    addMilliseconds(toDateTime64(session_start, 3, 'UTC'), t) AS wall_time,
    batch_seq,
    seq,
    t,
    name,
    concat(name, multiIf(
        name IN ('app.presented', 'app.idle'), concat(': ', ifNull(state, '?')),
        name = 'session.start', concat(': ', ifNull(`trigger`, '?')),
        name = 'workspace.enter', concat(': ', ifNull(workspace, '?')),
        name = 'interaction', concat(': ', ifNull(feature, '?')),
        name = 'tool.commit', concat(': ', ifNull(tool, '?')),
        name = 'operation.end', concat(': ', ifNull(operation, '?'), ' ', ifNull(outcome, '?')),
        name = 'preview.settled', concat(': ', ifNull(renderer, '?')),
        '')) AS label,
    toInt64(seq) - lagInFrame(toInt64(seq), 1, toInt64(-1)) OVER (PARTITION BY session_id ORDER BY seq ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) - 1 AS missing_before,
    image,
    feature,
    tool,
    operation,
    outcome,
    workspace,
    count,
    duration_ms,
    renderer,
    cpu_percent,
    memory_mib,
    memory_kind,
    gpu_percent,
    gpu_memory_mib,
    release
FROM luxforge.events_unique;

-- Active time per session, derived from app.presented and app.idle events only. A session starts
-- visible and active. Time counts while it is visible and not idle, between consecutive state events
-- and from the last state event to the last event of any kind that was received (the session's
-- tail beyond that is unknown and never counted).
CREATE VIEW IF NOT EXISTS luxforge.session_active_time AS
SELECT
    session_id,
    arrayFold(
        (acc, e) -> (
            if(e.3 = 'app.presented', toUInt8(e.4 = 'visible'), acc.1),
            if(e.3 = 'app.idle', toUInt8(e.4 = 'active'), acc.2),
            e.2,
            acc.4 + if(acc.1 = 1 AND acc.2 = 1, toUInt64(e.2 - acc.3), toUInt64(0))
        ),
        arrayPushBack(
            arraySort(x -> x.1, groupArrayIf((seq, t, name, ifNull(state, '')), name IN ('app.presented', 'app.idle'))),
            (toUInt32(4294967295), max(t), 'end', '')),
        (toUInt8(1), toUInt8(1), toUInt32(0), toUInt64(0))
    ).4 AS active_ms
FROM luxforge.events_unique
GROUP BY session_id;

-- One row per session: what was received, what is known to be missing, and its profile.
CREATE VIEW IF NOT EXISTS luxforge.session_summary AS
SELECT
    e.session_id AS session_id,
    e.installation_id AS installation_id,
    e.session_start AS session_start,
    e.events AS events,
    e.last_seq + 1 AS expected_events,
    e.last_seq + 1 - e.events AS missing_events,
    e.batches_with_events AS batches_with_events,
    ifNull(b.batches, 0) AS batches,
    ifNull(b.last_batch_seq, e.last_batch_seq) + 1 AS expected_batches,
    ifNull(b.last_batch_seq, e.last_batch_seq) + 1 - greatest(ifNull(b.batches, 0), e.batches_with_events) AS missing_batches,
    ifNull(b.dropped, 0) AS dropped,
    e.last_t AS last_t,
    e.first_received AS first_received,
    e.last_received AS last_received,
    ifNull(a.active_ms, 0) AS active_ms,
    p.release AS release, p.app AS app, p.os AS os, p.os_version_text AS os_version_text, p.arch AS arch,
    p.cpu AS cpu, p.memory_gib_band AS memory_gib_band, p.gpu AS gpu, p.renderer AS renderer,
    p.renderer_fallback AS renderer_fallback
FROM
(
    SELECT
        session_id,
        any(installation_id) AS installation_id,
        min(session_start) AS session_start,
        count() AS events,
        max(seq) AS last_seq,
        uniqExact(batch_seq) AS batches_with_events,
        max(batch_seq) AS last_batch_seq,
        max(t) AS last_t,
        min(received_at) AS first_received,
        max(received_at) AS last_received
    FROM luxforge.events_unique
    GROUP BY session_id
) AS e
LEFT JOIN
(
    SELECT session_id, count() AS batches, max(batch_seq) AS last_batch_seq, sum(dropped) AS dropped
    FROM luxforge.batches_unique
    GROUP BY session_id
) AS b ON b.session_id = e.session_id
LEFT JOIN luxforge.session_active_time AS a ON a.session_id = e.session_id
LEFT JOIN luxforge.session_profiles AS p ON p.session_id = e.session_id;
