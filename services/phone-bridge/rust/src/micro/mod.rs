//! Phone-owned emulated Micro wire formats and installed-client identity checks.
//! A report or matching driver inventory does not establish Desktop/Voice ownership.
mod attachment;
#[cfg(windows)]
mod client;
#[cfg(windows)]
mod client_files;
mod close;
mod command_output;
mod descriptors;
mod device;
mod gate;
mod inventory;
mod protocol;
mod report;
mod session;
mod transfer;
mod voice;

pub use attachment::{MicroAttachment, MicroClientResult, MicroCommand, MicroCommandError};
#[cfg(windows)]
pub use client::MicroUsbClient;
#[cfg(windows)]
pub use client_files::{MicroClientFileError, MicroClientFiles, MicroClientPins};
pub use close::{MicroClose, MicroCloseResult};
#[cfg(windows)]
pub use command_output::read_micro_console_output;
pub use command_output::{
    MicroOutputError, read_micro_command_output, read_micro_utf8_console_output,
};
pub use descriptors::{
    BUS_ID, DEVICE_ID, SERIAL, configuration, device, export, report_descriptor, string_descriptor,
};
pub use device::MicroUsbDevice;
pub use inventory::{InventoryError, own_port};
pub use protocol::{MicroProtocol, MicroProtocolError};
pub use report::{
    MAXIMUM_MESSAGE_BYTES, PAYLOAD_SIZE, REPORT_SIZE, Report, ReportError, frame, microphone,
    payload,
};
pub use session::{MicroClock, MicroDiagnostic, MicroStream, MicroUsbSession};
pub use transfer::{ControlTransfer, TransferError, control};
pub(crate) use voice::dispatch_micro_gesture_with_pause;
pub use voice::{
    MicroAttachmentStatus, MicroGestureResult, MicroInputReceipt, MicroInputResult,
    MicroVoiceInput, MicroVoiceObservation, dispatch_micro_gesture,
};

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_micro_wire.rs"]
mod tests;
