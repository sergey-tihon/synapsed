#![warn(missing_docs)]

//! Device-neutral state and native operation ownership. No hardware policy.
/// Native operation ownership primitives.
pub mod operation;
/// Device-neutral published state storage.
pub mod state;
/// Native operation authorization, lifecycle, and transport outcome types.
pub use operation::{
    DeviceGrant, EndpointGates, EndpointToken, NativeWork, OperationContext, OperationState,
    TransportOutcome,
};
/// Thread-safe, validated publication of daemon state.
pub use state::StateStore;
/// API error type exposed by the core crate.
pub use synapsed_api::{ApiError as DriverError, ErrorCode};
