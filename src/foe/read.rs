//! The FoE read (upload) transfer: a state machine with no I/O.
//!
//! Fed one mailbox reply at a time, it yields file content and leaves the next frame to send in
//! [`FoeRead::frame`]. Every transport detail — sync managers, polling, the mailbox counter —
//! belongs to the caller. Nothing is buffered: the request frame borrows the file name, and each
//! reply's content is borrowed straight out of the reply.

use crate::{
    error::FoeError,
    foe::{FoeFrame, FoeHeader, FoeOpcode, FOE_HEADER_LEN, MAX_BUSY_REPLIES},
};
use ethercrab_wire::EtherCrabWireRead;

/// What one reply advanced the transfer to. In every case the caller sends [`FoeRead::frame`] next;
/// only [`Progress::Last`] means stop afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress<'reply> {
    /// File content, with more to come.
    Chunk(&'reply [u8]),
    /// The content completing the file. Send the frame once more to acknowledge it, then stop.
    Last(&'reply [u8]),
    /// The SubDevice is still working; nothing was transferred.
    Busy,
}

/// The frame [`FoeRead`] wants to send next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The initial read request (names the file).
    Request,
    /// An acknowledgement of the packet whose number is carried alongside.
    Ack(u32),
}

/// One FoE read (upload) in progress.
pub struct FoeRead<'file> {
    file: &'file str,
    password: u32,
    /// File bytes one `Data` frame can carry. A shorter frame is the last one.
    max_data: usize,
    next_packet: u32,
    busy_replies: u32,
    phase: Phase,
}

impl<'file> FoeRead<'file> {
    /// Begin a read of `file`. `mailbox_payload` is the largest FoE service payload the SubDevice's
    /// mailbox carries — its sync-manager window less the 6-byte mailbox header — and is what makes
    /// a short `Data` recognisable as the last one.
    pub fn new(file: &'file str, password: u32, mailbox_payload: usize) -> Result<Self, FoeError> {
        let capacity = mailbox_payload.saturating_sub(FOE_HEADER_LEN);
        if file.len() > capacity {
            return Err(FoeError::FileNameTooLong {
                len: file.len(),
                capacity,
            });
        }

        Ok(Self {
            file,
            password,
            max_data: capacity,
            next_packet: 1,
            busy_replies: 0,
            phase: Phase::Request,
        })
    }

    /// The frame to send next: the read request before the first reply, and the acknowledgement of
    /// the most recent packet after each one.
    pub fn frame(&self) -> FoeFrame<'_> {
        match self.phase {
            Phase::Request => FoeFrame {
                header: FoeHeader {
                    opcode: FoeOpcode::ReadRequest,
                    argument: self.password,
                },
                body: self.file.as_bytes(),
            },
            Phase::Ack(packet) => FoeFrame {
                header: FoeHeader {
                    opcode: FoeOpcode::Ack,
                    argument: packet,
                },
                body: &[],
            },
        }
    }

    /// Feed one mailbox FoE payload, trimmed to the length its mailbox header declared. An untrimmed
    /// reply makes every packet look full and the transfer never ends.
    pub fn advance<'reply>(&mut self, reply: &'reply [u8]) -> Result<Progress<'reply>, FoeError> {
        let header = FoeHeader::unpack_from_slice(reply)
            .map_err(|_| FoeError::Truncated { len: reply.len() })?;
        let payload = reply
            .get(FOE_HEADER_LEN..)
            .ok_or(FoeError::Truncated { len: reply.len() })?;

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
            FoeOpcode::Data => {
                if header.argument != self.next_packet {
                    return Err(FoeError::PacketOutOfOrder {
                        expected: self.next_packet,
                        got: header.argument,
                    });
                }
                self.busy_replies = 0;
                self.next_packet += 1;
                self.phase = Phase::Ack(header.argument);

                if payload.len() < self.max_data {
                    Ok(Progress::Last(payload))
                } else {
                    Ok(Progress::Chunk(payload))
                }
            }
            opcode @ (FoeOpcode::ReadRequest | FoeOpcode::WriteRequest | FoeOpcode::Ack) => {
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

    fn reply(opcode: FoeOpcode, argument: u32, tail: &[u8]) -> heapless::Vec<u8, PAYLOAD> {
        let mut header = [0u8; FOE_HEADER_LEN];
        FoeHeader { opcode, argument }
            .pack_to_slice(&mut header)
            .unwrap();

        let mut frame = heapless::Vec::new();
        frame.extend_from_slice(&header).unwrap();
        frame.extend_from_slice(tail).unwrap();
        frame
    }

    fn data(packet: u32, len: usize) -> heapless::Vec<u8, PAYLOAD> {
        let tail = [0xAB; PER_PACKET];
        reply(FoeOpcode::Data, packet, &tail[..len])
    }

    fn transfer() -> FoeRead<'static> {
        FoeRead::new(".hardware_description", 0, PAYLOAD).unwrap()
    }

    #[test]
    fn the_first_request_names_the_file() {
        let read = transfer();
        assert_eq!(read.frame().header.opcode, FoeOpcode::ReadRequest);
        assert_eq!(read.frame().body, b".hardware_description");
    }

    #[test]
    fn a_full_packet_is_not_the_last_one() {
        let mut read = transfer();
        assert_eq!(
            read.advance(&data(1, PER_PACKET)).unwrap(),
            Progress::Chunk(&[0xAB; PER_PACKET][..])
        );
    }

    #[test]
    fn a_short_packet_ends_the_transfer() {
        let mut read = transfer();
        assert_eq!(
            read.advance(&data(1, 10)).unwrap(),
            Progress::Last(&[0xAB; 10][..])
        );
    }

    /// A file that is an exact multiple of the packet size ends with an empty `Data`, not with the
    /// last full one — the case that decides whether the transfer hangs waiting for a packet that
    /// never comes.
    #[test]
    fn an_exact_multiple_ends_on_an_empty_packet() {
        let mut read = transfer();
        assert!(matches!(
            read.advance(&data(1, PER_PACKET)).unwrap(),
            Progress::Chunk(_)
        ));
        assert_eq!(read.advance(&data(2, 0)).unwrap(), Progress::Last(&[][..]));
    }

    #[test]
    fn each_packet_is_acknowledged_by_number() {
        let mut read = transfer();
        let _ = read.advance(&data(1, PER_PACKET)).unwrap();

        assert_eq!(read.frame().header.opcode, FoeOpcode::Ack);
        assert_eq!(read.frame().header.argument, 1);
    }

    #[test]
    fn a_skipped_packet_is_refused_rather_than_stitched_over() {
        let mut read = transfer();
        let _ = read.advance(&data(1, PER_PACKET)).unwrap();

        assert_eq!(
            read.advance(&data(3, 10)),
            Err(FoeError::PacketOutOfOrder {
                expected: 2,
                got: 3
            })
        );
    }

    #[test]
    fn busy_asks_the_caller_to_try_again() {
        let mut read = transfer();
        assert_eq!(
            read.advance(&reply(FoeOpcode::Busy, 0, &[])).unwrap(),
            Progress::Busy
        );
        // The frame to resend is still the read request.
        assert_eq!(read.frame().header.opcode, FoeOpcode::ReadRequest);
    }

    #[test]
    fn endless_busy_gives_up() {
        let mut read = transfer();
        for _ in 0..MAX_BUSY_REPLIES {
            assert_eq!(
                read.advance(&reply(FoeOpcode::Busy, 0, &[])).unwrap(),
                Progress::Busy
            );
        }
        assert!(matches!(
            read.advance(&reply(FoeOpcode::Busy, 0, &[])),
            Err(FoeError::BusyTimeout { .. })
        ));
    }

    /// Busy is transient: a `Data` after one clears the count, so a long transfer that pauses
    /// repeatedly is not killed by the total.
    #[test]
    fn busy_resets_once_data_arrives() {
        let mut read = transfer();
        for _ in 0..MAX_BUSY_REPLIES {
            let _ = read.advance(&reply(FoeOpcode::Busy, 0, &[])).unwrap();
        }
        let _ = read.advance(&data(1, PER_PACKET)).unwrap();

        assert_eq!(
            read.advance(&reply(FoeOpcode::Busy, 0, &[])).unwrap(),
            Progress::Busy
        );
    }

    #[test]
    fn an_error_reply_carries_the_subdevices_code() {
        let mut read = transfer();
        assert_eq!(
            read.advance(&reply(FoeOpcode::Err, 0x8001, b"not found\0")),
            Err(FoeError::Refused { code: 0x8001 })
        );
    }

    #[test]
    fn a_reply_too_short_for_a_header_is_refused() {
        let mut read = transfer();
        assert_eq!(read.advance(&[3, 0]), Err(FoeError::Truncated { len: 2 }));
    }

    #[test]
    fn a_file_name_that_cannot_fit_the_mailbox_is_refused_up_front() {
        let name = "x".repeat(600);
        assert!(matches!(
            FoeRead::new(&name, 0, PAYLOAD),
            Err(FoeError::FileNameTooLong { .. })
        ));
    }

    /// A mailbox too small to hold even an FoE header must be refused, not underflowed into a
    /// nonsense capacity.
    #[test]
    fn a_mailbox_smaller_than_a_header_is_refused() {
        assert!(matches!(
            FoeRead::new(".hardware_description", 0, 4),
            Err(FoeError::FileNameTooLong { .. })
        ));
    }

    #[test]
    fn a_data_reply_before_any_request_number_matches_packet_one() {
        // The first Data must be packet 1; packet 2 up front is out of order.
        let mut read = transfer();
        assert_eq!(
            read.advance(&data(2, 4)),
            Err(FoeError::PacketOutOfOrder {
                expected: 1,
                got: 2
            })
        );
    }
}
