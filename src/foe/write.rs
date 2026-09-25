//! The FoE write (download) transfer: a state machine with no I/O.
//!
//! Fed one mailbox reply at a time, it stages the next frame to send in [`FoeWrite::frame`]. Every
//! transport detail — sync managers, polling, the mailbox counter — belongs to the caller. Nothing
//! is buffered: the request frame borrows the file name and each DATA frame borrows a slice of the
//! caller's data.
//!
//! The exchange is WRQ → ACK(0) → DATA(1) → ACK(1) → … → DATA(n) → ACK(n), where the final DATA is
//! shorter than a full packet. A file that is an exact multiple of the packet size therefore ends
//! with an empty DATA, so the SubDevice can always tell the last packet from a full one.

use crate::{
    error::FoeError,
    foe::{FoeFrame, FoeHeader, FoeOpcode, FOE_HEADER_LEN, MAX_BUSY_REPLIES},
};
use ethercrab_wire::EtherCrabWireRead;

/// What one reply advanced the transfer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Send [`FoeWrite::frame`] (the next DATA packet), then read the next reply.
    More,
    /// The final packet was acknowledged; the download is complete. Stop.
    Done,
    /// The SubDevice is still working; resend [`FoeWrite::frame`].
    Busy,
}

/// The frame [`FoeWrite`] wants to send next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The initial write request (names the file); the SubDevice acknowledges it as packet 0.
    Request,
    /// A data packet carrying `data[start..end]` as the given packet number.
    Data { packet: u32, start: usize, end: usize },
}

/// One FoE write (download) in progress.
pub struct FoeWrite<'file, 'data> {
    file: &'file str,
    password: u32,
    data: &'data [u8],
    /// File bytes one `Data` frame can carry. A shorter frame is the last one.
    max_data: usize,
    /// Bytes of `data` already staged into sent DATA frames.
    offset: usize,
    next_packet: u32,
    busy_replies: u32,
    phase: Phase,
}

impl<'file, 'data> FoeWrite<'file, 'data> {
    /// Begin a write of `data` to `file`. `mailbox_payload` is the largest FoE service payload the
    /// SubDevice's mailbox carries — its sync-manager window less the 6-byte mailbox header.
    pub fn new(
        file: &'file str,
        password: u32,
        data: &'data [u8],
        mailbox_payload: usize,
    ) -> Result<Self, FoeError> {
        let capacity = mailbox_payload.saturating_sub(FOE_HEADER_LEN);
        // A zero payload capacity can make no progress on a non-empty file; the file-name check
        // rejects it for any real transfer (FoE always names a file), and this covers the rest.
        if file.len() > capacity || (capacity == 0 && !data.is_empty()) {
            return Err(FoeError::FileNameTooLong {
                len: file.len(),
                capacity,
            });
        }

        Ok(Self {
            file,
            password,
            data,
            max_data: capacity,
            offset: 0,
            next_packet: 1,
            busy_replies: 0,
            phase: Phase::Request,
        })
    }

    /// The frame to send next: the write request before the first reply, then each data packet.
    pub fn frame(&self) -> FoeFrame<'_> {
        match self.phase {
            Phase::Request => FoeFrame {
                header: FoeHeader {
                    opcode: FoeOpcode::WriteRequest,
                    argument: self.password,
                },
                body: self.file.as_bytes(),
            },
            Phase::Data { packet, start, end } => FoeFrame {
                header: FoeHeader {
                    opcode: FoeOpcode::Data,
                    argument: packet,
                },
                body: &self.data[start..end],
            },
        }
    }

    /// Feed one mailbox FoE payload. A well-behaved SubDevice answers each sent frame with an `Ack`
    /// carrying the same packet number (the write request is packet 0).
    pub fn advance(&mut self, reply: &[u8]) -> Result<Progress, FoeError> {
        let header = FoeHeader::unpack_from_slice(reply)
            .map_err(|_| FoeError::Truncated { len: reply.len() })?;

        match header.opcode {
            FoeOpcode::Busy => {
                self.busy_replies += 1;
                if self.busy_replies > MAX_BUSY_REPLIES {
                    return Err(FoeError::BusyTimeout {
                        replies: self.busy_replies,
                    });
                }
                Ok(Progress::Busy)
            }
            FoeOpcode::Err => Err(FoeError::Refused {
                code: header.argument,
            }),
            FoeOpcode::Ack => {
                let expected = match self.phase {
                    Phase::Request => 0,
                    Phase::Data { packet, .. } => packet,
                };
                if header.argument != expected {
                    return Err(FoeError::PacketOutOfOrder {
                        expected,
                        got: header.argument,
                    });
                }
                self.busy_replies = 0;

                // The last DATA is the one shorter than a full packet; the write request is never
                // terminal (there is always at least one DATA, empty for an empty file).
                let was_terminal = matches!(
                    self.phase,
                    Phase::Data { start, end, .. } if end - start < self.max_data
                );
                if was_terminal {
                    return Ok(Progress::Done);
                }

                let start = self.offset;
                let end = start + (self.data.len() - start).min(self.max_data);
                self.offset = end;
                let packet = self.next_packet;
                self.next_packet += 1;
                self.phase = Phase::Data { packet, start, end };

                Ok(Progress::More)
            }
            opcode @ (FoeOpcode::ReadRequest | FoeOpcode::WriteRequest | FoeOpcode::Data) => {
                Err(FoeError::UnexpectedOpcode(opcode))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethercrab_wire::EtherCrabWireWrite;

    /// One mailbox holds 506 payload bytes, so 500 file bytes per `Data`.
    const PAYLOAD: usize = 506;
    const PER_PACKET: usize = PAYLOAD - FOE_HEADER_LEN;

    fn reply(opcode: FoeOpcode, argument: u32) -> [u8; FOE_HEADER_LEN] {
        let mut buf = [0u8; FOE_HEADER_LEN];
        FoeHeader { opcode, argument }
            .pack_to_slice(&mut buf)
            .unwrap();
        buf
    }

    fn ack(packet: u32) -> [u8; FOE_HEADER_LEN] {
        reply(FoeOpcode::Ack, packet)
    }

    fn transfer(data: &[u8]) -> FoeWrite<'static, '_> {
        FoeWrite::new("firmware.bin", 0, data, PAYLOAD).unwrap()
    }

    #[test]
    fn the_first_request_names_the_file() {
        let data = [0u8; 10];
        let write = transfer(&data);
        assert_eq!(write.frame().header.opcode, FoeOpcode::WriteRequest);
        assert_eq!(write.frame().body, b"firmware.bin");
    }

    #[test]
    fn the_write_request_is_acknowledged_as_packet_zero_then_data_one_is_sent() {
        let data = [0xAB; PER_PACKET];
        let mut write = transfer(&data);

        assert_eq!(write.advance(&ack(0)).unwrap(), Progress::More);
        assert_eq!(write.frame().header.opcode, FoeOpcode::Data);
        assert_eq!(write.frame().header.argument, 1);
        assert_eq!(write.frame().body, &data[..]);
    }

    #[test]
    fn a_short_final_packet_completes_after_its_ack() {
        let data = [0xCD; 40];
        let mut write = transfer(&data);

        assert_eq!(write.advance(&ack(0)).unwrap(), Progress::More);
        // DATA 1 is short (< PER_PACKET), so it is the last packet.
        assert_eq!(write.frame().body, &data[..]);
        assert_eq!(write.advance(&ack(1)).unwrap(), Progress::Done);
    }

    #[test]
    fn a_two_packet_transfer_sends_each_slice_in_order() {
        let mut data = [0u8; PER_PACKET + 40];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut write = transfer(&data);

        assert_eq!(write.advance(&ack(0)).unwrap(), Progress::More);
        assert_eq!(write.frame().header.argument, 1);
        assert_eq!(write.frame().body, &data[..PER_PACKET]);

        assert_eq!(write.advance(&ack(1)).unwrap(), Progress::More);
        assert_eq!(write.frame().header.argument, 2);
        assert_eq!(write.frame().body, &data[PER_PACKET..]);

        assert_eq!(write.advance(&ack(2)).unwrap(), Progress::Done);
    }

    /// A file that is an exact multiple of the packet size must end with an empty DATA, otherwise
    /// the SubDevice cannot tell the last full packet from a mid-transfer one.
    #[test]
    fn an_exact_multiple_ends_on_an_empty_packet() {
        let data = [0xEE; PER_PACKET];
        let mut write = transfer(&data);

        assert_eq!(write.advance(&ack(0)).unwrap(), Progress::More);
        assert_eq!(write.frame().body.len(), PER_PACKET);

        // The full packet is not the end; an empty DATA 2 follows.
        assert_eq!(write.advance(&ack(1)).unwrap(), Progress::More);
        assert_eq!(write.frame().header.argument, 2);
        assert_eq!(write.frame().body.len(), 0);

        assert_eq!(write.advance(&ack(2)).unwrap(), Progress::Done);
    }

    #[test]
    fn an_empty_file_sends_a_single_empty_data_packet() {
        let mut write = transfer(&[]);

        assert_eq!(write.advance(&ack(0)).unwrap(), Progress::More);
        assert_eq!(write.frame().header.opcode, FoeOpcode::Data);
        assert_eq!(write.frame().header.argument, 1);
        assert_eq!(write.frame().body.len(), 0);

        assert_eq!(write.advance(&ack(1)).unwrap(), Progress::Done);
    }

    #[test]
    fn an_ack_for_the_wrong_packet_is_refused() {
        let data = [0u8; 40];
        let mut write = transfer(&data);

        // Expecting ack 0 for the write request.
        assert_eq!(
            write.advance(&ack(5)),
            Err(FoeError::PacketOutOfOrder {
                expected: 0,
                got: 5
            })
        );
    }

    #[test]
    fn an_error_reply_aborts_with_the_subdevices_code() {
        let data = [0u8; 40];
        let mut write = transfer(&data);

        assert_eq!(
            write.advance(&reply(FoeOpcode::Err, 0x8010)),
            Err(FoeError::Refused { code: 0x8010 })
        );
    }

    #[test]
    fn busy_asks_the_caller_to_resend_the_current_frame() {
        let data = [0u8; 40];
        let mut write = transfer(&data);

        assert_eq!(write.advance(&reply(FoeOpcode::Busy, 0)).unwrap(), Progress::Busy);
        // Still the write request, unchanged, ready to resend.
        assert_eq!(write.frame().header.opcode, FoeOpcode::WriteRequest);
    }

    #[test]
    fn endless_busy_gives_up() {
        let data = [0u8; 40];
        let mut write = transfer(&data);

        for _ in 0..MAX_BUSY_REPLIES {
            assert_eq!(write.advance(&reply(FoeOpcode::Busy, 0)).unwrap(), Progress::Busy);
        }
        assert!(matches!(
            write.advance(&reply(FoeOpcode::Busy, 0)),
            Err(FoeError::BusyTimeout { .. })
        ));
    }

    #[test]
    fn a_data_reply_is_an_unexpected_opcode() {
        let data = [0u8; 40];
        let mut write = transfer(&data);

        assert_eq!(
            write.advance(&reply(FoeOpcode::Data, 1)),
            Err(FoeError::UnexpectedOpcode(FoeOpcode::Data))
        );
    }

    #[test]
    fn a_file_name_that_cannot_fit_the_mailbox_is_refused_up_front() {
        let name = "x".repeat(600);
        assert!(matches!(
            FoeWrite::new(&name, 0, &[0u8; 4], PAYLOAD),
            Err(FoeError::FileNameTooLong { .. })
        ));
    }
}
