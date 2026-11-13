//! Deterministic developer benchmarks, not model-performance evaluations.
//! Capture never opens an oracle; downloaded bytes are only parsed or hashed.
pub mod common;
pub mod event_stream;
pub mod south_africa;
pub mod transport;
