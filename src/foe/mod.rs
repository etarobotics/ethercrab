//! File over EtherCAT (FoE), ETG.1000.6 §5.5.
//!
//! FoE is a lock-step, MainDevice-driven file transfer over the mailbox SyncManagers. This module
//! holds the wire types ([`FoeHeader`], [`FoeOpcode`]) and two pure, no-I/O state machines —
//! [`read::FoeRead`] (upload, SubDevice → MainDevice) and [`write::FoeWrite`] (download,
//! MainDevice → SubDevice). Each is fed one mailbox reply at a time and stages the next frame to
//! send; every transport detail (sync managers, polling, the mailbox counter) belongs to the
//! caller. [`SubDeviceRef::foe_read`](crate::SubDeviceRef::foe_read) and
//! [`SubDeviceRef::foe_write`](crate::SubDeviceRef::foe_write) drive them over the raw mailbox.

use ethercrab_wire::{EtherCrabWireSized, EtherCrabWireWrite};

pub mod read;
pub mod write;

/// Bytes of FoE header ahead of every FoE payload.
pub const FOE_HEADER_LEN: usize = FoeHeader::PACKED_LEN;

/// Consecutive `Busy` replies tolerated before giving up. A SubDevice sends one while it is still
/// working, so a few are ordinary; this many means it never intends to answer.
pub const MAX_BUSY_REPLIES: u32 = 100;

/// FoE opcode (ETG.1000.6 §5.5, Table 96).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum FoeOpcode {
    /// Read request (RRQ): MainDevice asks to upload a file.
    ReadRequest = 1,
    /// Write request (WRQ): MainDevice asks to download a file.
    WriteRequest = 2,
    /// Data (DATA): one numbered chunk of file content.
    Data = 3,
    /// Acknowledgement (ACK): confirms a numbered packet.
    Ack = 4,
    /// Error (ERR): the transfer is refused or aborted.
    // Named `Err`, not `Error`: a variant named `Error` collides with `TryFrom::Error` in the
    // wire-derive-generated code (same reason `MailboxType::Err` is spelled this way).
    Err = 5,
    /// Busy (BUSY): the SubDevice is still working; retry.
    Busy = 6,
}

/// The six bytes ahead of every FoE payload (ETG.1000.6 §5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[wire(bytes = 6)]
pub struct FoeHeader {
    /// The opcode identifying this frame.
    // Byte 1 is reserved and always zero.
    #[wire(bytes = 1, post_skip_bytes = 1)]
    pub opcode: FoeOpcode,
    /// Password on a read/write request, packet number on data and ack, error code on an error,
    /// and the completed-byte count on busy.
    #[wire(bytes = 4)]
    pub argument: u32,
}

/// An FoE frame ready to send: the FoE header plus a borrowed body, packed in place.
///
/// Used as the body of a [`MailboxFrame`](crate::MailboxFrame) so a write DATA frame's file chunk
/// is a borrow of the caller's data — the mailbox header, FoE header, and chunk all pack straight
/// into the send frame with no intermediate buffer.
#[derive(Clone, Copy, Debug)]
pub struct FoeFrame<'a> {
    /// The 6-byte FoE header.
    pub header: FoeHeader,
    /// The FoE payload: a file name (RRQ/WRQ), a file chunk (DATA), or empty (ACK).
    pub body: &'a [u8],
}

impl EtherCrabWireWrite for FoeFrame<'_> {
    fn pack_to_slice_unchecked<'buf>(&self, buf: &'buf mut [u8]) -> &'buf [u8] {
        let header_len = FOE_HEADER_LEN;
        let end = header_len + self.body.len();

        self.header.pack_to_slice_unchecked(&mut buf[..header_len]);
        buf[header_len..end].copy_from_slice(self.body);

        &buf[..end]
    }

    fn packed_len(&self) -> usize {
        FOE_HEADER_LEN + self.body.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethercrab_wire::EtherCrabWireRead;

    #[test]
    fn header_is_six_bytes() {
        assert_eq!(FoeHeader::PACKED_LEN, 6);
    }

    #[test]
    fn read_request_header_layout() {
        let header = FoeHeader {
            opcode: FoeOpcode::ReadRequest,
            argument: 0xDEAD_BEEF,
        };

        let mut buf = [0u8; 6];
        header.pack_to_slice(&mut buf).unwrap();

        assert_eq!(buf[0], 1);
        // Byte 1 is reserved and must be written as zero.
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..], &0xDEAD_BEEFu32.to_le_bytes());

        assert_eq!(FoeHeader::unpack_from_slice(&buf).unwrap(), header);
    }

    #[test]
    fn unknown_opcode_is_refused() {
        // 0 and 7 are not opcodes; decoding either must fail rather than guess.
        assert!(FoeHeader::unpack_from_slice(&[0u8, 0, 0, 0, 0, 0]).is_err());
        assert!(FoeHeader::unpack_from_slice(&[7u8, 0, 0, 0, 0, 0]).is_err());
    }
}
