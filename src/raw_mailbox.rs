//! Raw ETG.1000.4 mailbox channel: a framed, protocol-agnostic byte pipe over the mailbox
//! SyncManagers, independent of CoE/SDO.
//!
//! [`crate::mailbox::MailboxHeader`] is *not* usable here: it is CoE-coupled, folding the CoE
//! header's `service` nibble into an 8-byte struct whose `length` is measured from the true
//! 6-byte boundary. [`MailboxHeader`] below is the pure ETG.1000.4 mailbox header, so every byte
//! after it is caller-owned payload.

use crate::{
    error::{Error, MailboxError},
    mailbox::{MailboxType, Priority},
};
use ethercrab_wire::{EtherCrabWireRead, EtherCrabWireSized, EtherCrabWireWrite};

/// Largest mailbox frame (6-byte header + body) this channel supports, i.e. the largest mailbox SM
/// window it can drive. Sized to the window provisioned in the SII. The usable body is this minus
/// [`MailboxHeader::PACKED_LEN`].
pub const MAILBOX_MAX_LEN: usize = 512;

/// The 6-byte ETG.1000.4 mailbox header (ETG.1000.4 §5.6). Unlike
/// [`crate::mailbox::MailboxHeader`] this carries no CoE `service` field — the payload following it
/// belongs entirely to the mailbox protocol chosen by [`mailbox_type`](Self::mailbox_type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[wire(bytes = 6)]
pub struct MailboxHeader {
    /// Length in bytes of the mailbox data that follows this header.
    // The 2-byte Address field (bytes 2..4) is always zero while the master is in control, so it is
    // skipped rather than represented.
    #[wire(bytes = 2, post_skip_bytes = 2)]
    pub length: u16,
    /// Mailbox priority (rarely meaningful for a single-mailbox device; sent as
    /// [`Priority::Lowest`]).
    // Byte 4: Channel (6 bits, unused → skipped) then Priority (2 bits).
    #[wire(pre_skip = 6, bits = 2)]
    pub priority: Priority,
    /// Which mailbox protocol the body carries (this channel uses
    /// [`MailboxType::VendorSpecific`]).
    // Byte 5: Type (4 bits) + Counter (3 bits) + 1 reserved bit.
    #[wire(bits = 4)]
    pub mailbox_type: MailboxType,
    /// Cyclic mailbox counter, 1..=7. Retransmits reuse the value so the SubDevice can dedup.
    #[wire(bits = 3, post_skip = 1)]
    pub counter: u8,
}

/// A mailbox message: the parsed [`MailboxHeader`] plus its raw body (already truncated to
/// [`header.length`](MailboxHeader::length)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxMessage {
    /// The 6-byte ETG.1000.4 header. `header.mailbox_type` and `header.counter` identify the
    /// message; `header.length == body.len()`.
    pub header: MailboxHeader,
    /// Raw mailbox data (the bytes after the header).
    pub body: heapless::Vec<u8, MAILBOX_MAX_LEN>,
}

// Buffer/window length errors reuse `MailboxError::TooLong`; the CoE-oriented address/sub_index
// fields have no meaning for a raw frame, so they are zeroed.
fn too_long() -> Error {
    Error::Mailbox(MailboxError::TooLong {
        address: 0,
        sub_index: 0,
    })
}

/// Build `[header][data]` into `out`, returning the frame length. `out` is the mailbox SM window
/// slice, so `out.len()` bounds the frame (writing past the window would corrupt adjacent DPRAM);
/// only the header + data are written and the send zero-pads the datagram to the window.
pub fn build_frame(
    counter: u8,
    mailbox_type: MailboxType,
    data: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = MailboxHeader::PACKED_LEN + data.len();

    if total > MAILBOX_MAX_LEN || out.len() < total {
        return Err(too_long());
    }

    let header = MailboxHeader {
        length: data.len() as u16,
        priority: Priority::Lowest,
        mailbox_type,
        counter,
    };

    header.pack_to_slice(&mut out[..MailboxHeader::PACKED_LEN])?;
    out[MailboxHeader::PACKED_LEN..total].copy_from_slice(data);

    Ok(total)
}

/// Parse a frame read out of a mailbox window, truncating the body to the header's declared length.
pub fn parse_frame(window: &[u8]) -> Result<MailboxMessage, Error> {
    let header = MailboxHeader::unpack_from_slice(
        window.get(..MailboxHeader::PACKED_LEN).ok_or_else(too_long)?,
    )?;

    let len = usize::from(header.length);
    let start = MailboxHeader::PACKED_LEN;

    let body_bytes = window.get(start..start + len).ok_or_else(too_long)?;

    let mut body = heapless::Vec::new();
    body.extend_from_slice(body_bytes).map_err(|_| too_long())?;

    Ok(MailboxMessage { header, body })
}
