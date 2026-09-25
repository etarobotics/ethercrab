//! Raw ETG.1000.4 mailbox channel: a framed, protocol-agnostic byte pipe over the mailbox
//! SyncManagers, independent of CoE/SDO.
//!
//! [`crate::mailbox::MailboxHeader`] is *not* usable here: it is CoE-coupled, folding the CoE
//! header's `service` nibble into an 8-byte struct whose `length` is measured from the true
//! 6-byte boundary. [`MailboxHeader`] below is the pure ETG.1000.4 mailbox header, so every byte
//! after it is caller-owned payload.
//!
//! Both directions are zero-copy against the [`PduStorage`](crate::PduStorage) frame the caller
//! already provisions: an outgoing [`MailboxFrame`] packs its header and body straight into the
//! send frame, and an incoming [`MailboxMessage`] borrows the received frame rather than copying it
//! out. The mailbox window is therefore bounded by `MAX_PDU_DATA`, not a compile-time constant here.

use crate::{
    error::{Error, MailboxError},
    mailbox::{MailboxType, Priority},
    pdu_loop::ReceivedPdu,
};
use ethercrab_wire::{EtherCrabWireRead, EtherCrabWireSized, EtherCrabWireWrite};

/// The 6-byte ETG.1000.4 mailbox header (ETG.1000.4 §5.6). Unlike
/// [`crate::mailbox::MailboxHeader`] this carries no CoE `service` field — the payload following it
/// belongs entirely to the mailbox protocol chosen by [`mailbox_type`](Self::mailbox_type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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
    /// Which mailbox protocol the body carries.
    // Byte 5: Type (4 bits) + Counter (3 bits) + 1 reserved bit.
    #[wire(bits = 4)]
    pub mailbox_type: MailboxType,
    /// Cyclic mailbox counter, 1..=7. Retransmits reuse the value so the SubDevice can dedup.
    #[wire(bits = 3, post_skip = 1)]
    pub counter: u8,
}

/// An outgoing mailbox frame: a header plus a body, packed in place into the send buffer.
///
/// The body is any [`EtherCrabWireWrite`] — a raw `&[u8]`, or a typed protocol frame (e.g. an FoE
/// frame) that itself packs a sub-header plus payload. Because everything packs directly into the
/// send frame there is no intermediate scratch buffer. The send zero-pads the datagram out to the
/// mailbox SM window (via `with_len`) so the ESC flips the SM full on the last byte.
#[derive(Clone, Copy, Debug)]
pub struct MailboxFrame<B> {
    /// The 6-byte header. `header.length` must equal the body's packed length.
    pub header: MailboxHeader,
    /// The mailbox body (bytes after the header).
    pub body: B,
}

impl<B: EtherCrabWireWrite> EtherCrabWireWrite for MailboxFrame<B> {
    fn pack_to_slice_unchecked<'buf>(&self, buf: &'buf mut [u8]) -> &'buf [u8] {
        let header_len = MailboxHeader::PACKED_LEN;

        self.header.pack_to_slice_unchecked(&mut buf[..header_len]);
        let body_len = self.body.pack_to_slice_unchecked(&mut buf[header_len..]).len();

        &buf[..header_len + body_len]
    }

    fn packed_len(&self) -> usize {
        MailboxHeader::PACKED_LEN + self.body.packed_len()
    }
}

/// A received mailbox message: the parsed [`MailboxHeader`] and a zero-copy view of its body.
///
/// Holds the [`ReceivedPdu`] the frame arrived in, so [`body`](Self::body) borrows the frame
/// buffer directly. Consume it promptly: like any `ReceivedPdu`, the underlying [`PduStorage`] slot
/// is released as soon as the frame is read, so holding a `MailboxMessage` across another bus
/// operation risks the slot being reused underneath it.
///
/// [`PduStorage`]: crate::PduStorage
pub struct MailboxMessage<'sto> {
    header: MailboxHeader,
    pdu: ReceivedPdu<'sto>,
}

impl<'sto> MailboxMessage<'sto> {
    /// Parse a mailbox frame read out of a mailbox SM window, validating that the declared body
    /// length fits within the received frame.
    pub(crate) fn parse(pdu: ReceivedPdu<'sto>) -> Result<Self, Error> {
        // Validate against the received bytes; discard the borrow, keep the (Copy) header.
        let (header, _body) = parse_frame(&pdu)?;

        Ok(Self { header, pdu })
    }

    /// The parsed 6-byte mailbox header.
    pub fn header(&self) -> &MailboxHeader {
        &self.header
    }

    /// The mailbox protocol this message carries.
    pub fn mailbox_type(&self) -> MailboxType {
        self.header.mailbox_type
    }

    /// The cyclic mailbox counter this message arrived with.
    pub fn counter(&self) -> u8 {
        self.header.counter
    }

    /// The mailbox body: the bytes after the header, trimmed to the header's declared length.
    pub fn body(&self) -> &[u8] {
        let start = MailboxHeader::PACKED_LEN;
        let end = start + usize::from(self.header.length);

        // Bounds were validated in `parse`.
        &self.pdu[start..end]
    }
}

// Buffer/window length errors reuse `MailboxError::TooLong`; the CoE-oriented address/sub_index
// fields have no meaning for a raw frame, so they are zeroed.
fn too_long() -> Error {
    Error::Mailbox(MailboxError::TooLong {
        address: 0,
        sub_index: 0,
    })
}

/// Parse a mailbox `window`, returning the header and a borrowed body slice trimmed to the header's
/// declared length. Trailing window padding past that length is not part of the message.
pub fn parse_frame(window: &[u8]) -> Result<(MailboxHeader, &[u8]), Error> {
    let header = MailboxHeader::unpack_from_slice(
        window.get(..MailboxHeader::PACKED_LEN).ok_or_else(too_long)?,
    )?;

    let start = MailboxHeader::PACKED_LEN;
    let end = start + usize::from(header.length);

    let body = window.get(start..end).ok_or_else(too_long)?;

    Ok((header, body))
}
