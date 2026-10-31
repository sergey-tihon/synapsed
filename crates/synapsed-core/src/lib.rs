//! Device-neutral state and native operation ownership. No hardware policy.
pub mod operation;
pub mod state;
pub use operation::{
    DeviceGrant, EndpointGates, EndpointToken, NativeWork, OperationContext, OperationState,
    TransportOutcome,
};
pub use state::StateStore;
pub use synapsed_api::{ApiError as DriverError, ErrorCode};
