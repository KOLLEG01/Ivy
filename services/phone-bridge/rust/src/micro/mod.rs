//! Phone-owned emulated Micro wire formats and installed-client identity checks.
//! A report or matching driver inventory does not establish Desktop/Voice ownership.
mod descriptors;
mod inventory;
mod report;
mod transfer;

pub use descriptors::{
    BUS_ID, DEVICE_ID, SERIAL, configuration, device, export, report_descriptor, string_descriptor,
};
pub use inventory::{InventoryError, own_port};
pub use report::{
    MAXIMUM_MESSAGE_BYTES, PAYLOAD_SIZE, REPORT_SIZE, Report, ReportError, frame, microphone,
    payload,
};
pub use transfer::{ControlTransfer, TransferError, control};

#[cfg(all(test, windows))]
mod tests;
