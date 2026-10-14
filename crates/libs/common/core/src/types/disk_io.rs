use std::num::NonZeroU32;

/// Default per-sandbox disk operation rate for new unassigned sandboxes.
/// Small-file workloads consume thousands of guest requests even when the
/// backing image is hot in the host cache. Physical host protection belongs
/// at the parent cgroup, where cache hits do not spend device I/O capacity.
pub const DEFAULT_DISK_IO_MAX_OPERATIONS_PER_SEC: u64 = 1_000;

/// Default per-sandbox disk bandwidth for new unassigned sandboxes.
pub const DEFAULT_DISK_IO_MAX_BANDWIDTH_MIB_PER_SEC: u64 = 100;

const DEFAULT_OPERATIONS_ENV: &str = "SPACE_DISK_IO_DEFAULT_MAX_OPERATIONS_PER_SEC";
const DEFAULT_BANDWIDTH_ENV: &str = "SPACE_DISK_IO_DEFAULT_MAX_BANDWIDTH_MIB_PER_SEC";

/// Default seconds of its sustained rates a sandbox drive's buckets hold.
/// Guest disk requests are mostly one 4 KiB page (the virtio disk takes one
/// segment per request), so a command that reads a cold Python install or
/// copies a small tree issues thousands of operations at once, and uploads
/// arrive in bursts. The buckets let a sandbox that was idle spend that much
/// at once, as fast as the disk takes it; the sustained rates, and so a
/// sandbox's long-run share of the node's shared disk, stay those of
/// [`DiskIoLimits`].
pub const DEFAULT_DISK_IO_BURST_SECS: NonZeroU32 = NonZeroU32::new(30).unwrap();

/// A Firecracker token bucket: `size` tokens, refilled from empty over
/// `refill_time_ms`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenBucket {
    pub size: u64,
    pub refill_time_ms: u64,
}

/// A Firecracker device rate limiter: one token bucket for bytes and one for
/// operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimiter {
    pub bandwidth: TokenBucket,
    pub operations: TokenBucket,
}

impl RateLimiter {
    /// The limiter enforcing a sandbox drive's [`DiskIoLimits`]. Each bucket
    /// holds `burst_secs` of its sustained rate and refills at that rate.
    pub const fn for_disk(limits: DiskIoLimits, burst_secs: NonZeroU32) -> Self {
        let burst_secs = burst_secs.get() as u64;
        Self {
            bandwidth: TokenBucket {
                size: limits
                    .max_bandwidth_bytes_per_sec()
                    .saturating_mul(burst_secs),
                refill_time_ms: burst_secs * 1000,
            },
            operations: TokenBucket {
                size: limits.max_operations_per_sec.saturating_mul(burst_secs),
                refill_time_ms: burst_secs * 1000,
            },
        }
    }
}

const MIB_BYTES: u64 = 1024 * 1024;
const MAX_OPERATIONS_PER_SEC: u64 = i64::MAX as u64;
const MAX_BANDWIDTH_MIB_PER_SEC: u64 = u64::MAX / MIB_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskIoLimits {
    max_operations_per_sec: u64,
    max_bandwidth_mib_per_sec: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiskIoLimitsError {
    #[error("cannot read disk I/O default {name}: {source}")]
    Environment {
        name: &'static str,
        #[source]
        source: std::env::VarError,
    },
    #[error("disk I/O default {name} must be a positive integer: {source}")]
    ParseEnvironment {
        name: &'static str,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("disk I/O {field} must be greater than zero")]
    Zero { field: &'static str },
    #[error("disk I/O {field} value {value} exceeds the supported maximum {maximum}")]
    OutOfRange {
        field: &'static str,
        value: u64,
        maximum: u64,
    },
    #[error("disk I/O operations and bandwidth must either both be present or both be absent")]
    PartialPair,
}

impl DiskIoLimits {
    /// Deployment default for new assignments and legacy metadata without an
    /// assignment. Set both environment values or neither, on every writer and
    /// worker. Unlike `Default`, this boundary is fallible and honors operator
    /// configuration. Explicit persisted limits never pass through it.
    pub fn configured_default() -> Result<Self, DiskIoLimitsError> {
        let read = |name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(source) => Err(DiskIoLimitsError::Environment { name, source }),
        };
        Self::default_from_values(
            read(DEFAULT_OPERATIONS_ENV)?.as_deref(),
            read(DEFAULT_BANDWIDTH_ENV)?.as_deref(),
        )
    }

    /// Validate and report the deployment default once during service startup.
    pub fn configured_default_at_startup() -> Result<Self, DiskIoLimitsError> {
        let limits = Self::configured_default()?;
        let source = if std::env::var_os(DEFAULT_OPERATIONS_ENV).is_some() {
            "environment"
        } else {
            "shipped"
        };
        tracing::info!(
            max_operations_per_sec = limits.max_operations_per_sec(),
            max_bandwidth_mib_per_sec = limits.max_bandwidth_mib_per_sec(),
            source,
            "resolved deployment disk I/O default"
        );
        Ok(limits)
    }

    fn default_from_values(
        operations: Option<&str>,
        bandwidth: Option<&str>,
    ) -> Result<Self, DiskIoLimitsError> {
        let parse = |name, raw: &str| {
            raw.trim()
                .parse()
                .map_err(|source| DiskIoLimitsError::ParseEnvironment { name, source })
        };
        match (operations, bandwidth) {
            (None, None) => Ok(Self::default()),
            (Some(operations), Some(bandwidth)) => Self::try_new(
                parse(DEFAULT_OPERATIONS_ENV, operations)?,
                parse(DEFAULT_BANDWIDTH_ENV, bandwidth)?,
            ),
            _ => Err(DiskIoLimitsError::PartialPair),
        }
    }
    pub fn try_new(
        max_operations_per_sec: u64,
        max_bandwidth_mib_per_sec: u64,
    ) -> Result<Self, DiskIoLimitsError> {
        validate_positive("max_operations_per_sec", max_operations_per_sec)?;
        validate_positive("max_bandwidth_mib_per_sec", max_bandwidth_mib_per_sec)?;
        validate_maximum(
            "max_operations_per_sec",
            max_operations_per_sec,
            MAX_OPERATIONS_PER_SEC,
        )?;
        validate_maximum(
            "max_bandwidth_mib_per_sec",
            max_bandwidth_mib_per_sec,
            MAX_BANDWIDTH_MIB_PER_SEC,
        )?;
        Ok(Self {
            max_operations_per_sec,
            max_bandwidth_mib_per_sec,
        })
    }

    /// Resolves an optional wire pair against the platform defaults. Older
    /// writers omit both fields; a partial pair is malformed rather than a
    /// mixed-version state and must not silently acquire one default value.
    pub fn try_from_optional(
        max_operations_per_sec: Option<u64>,
        max_bandwidth_mib_per_sec: Option<u64>,
    ) -> Result<Self, DiskIoLimitsError> {
        match (max_operations_per_sec, max_bandwidth_mib_per_sec) {
            (None, None) => Self::configured_default(),
            (Some(operations), Some(bandwidth)) => Self::try_new(operations, bandwidth),
            _ => Err(DiskIoLimitsError::PartialPair),
        }
    }

    pub const fn max_operations_per_sec(self) -> u64 {
        self.max_operations_per_sec
    }

    pub const fn max_bandwidth_mib_per_sec(self) -> u64 {
        self.max_bandwidth_mib_per_sec
    }

    /// The bandwidth limit in bytes; validation keeps it representable.
    pub const fn max_bandwidth_bytes_per_sec(self) -> u64 {
        self.max_bandwidth_mib_per_sec * MIB_BYTES
    }
}

impl Default for DiskIoLimits {
    fn default() -> Self {
        Self {
            max_operations_per_sec: DEFAULT_DISK_IO_MAX_OPERATIONS_PER_SEC,
            max_bandwidth_mib_per_sec: DEFAULT_DISK_IO_MAX_BANDWIDTH_MIB_PER_SEC,
        }
    }
}

fn validate_positive(field: &'static str, value: u64) -> Result<(), DiskIoLimitsError> {
    if value == 0 {
        return Err(DiskIoLimitsError::Zero { field });
    }
    Ok(())
}

fn validate_maximum(
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), DiskIoLimitsError> {
    if value > maximum {
        return Err(DiskIoLimitsError::OutOfRange {
            field,
            value,
            maximum,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
