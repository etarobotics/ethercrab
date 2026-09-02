//! Round-trip + wire-layout tests for the raw ETG.1000.4 mailbox frame codec.
//!
//! These live as an integration test (not a `#[cfg(test)]` module) so they compile against the
//! library alone — the crate's internal unit tests `include_bytes!` EEPROM fixtures that the
//! published package excludes and so do not build in a vendored checkout.

use ethercrab::raw_mailbox::{build_frame, parse_frame};
use ethercrab::{MailboxHeader, MailboxType, Priority, MAILBOX_MAX_LEN};
use ethercrab_wire::{EtherCrabWireRead, EtherCrabWireSized, EtherCrabWireWrite};

#[test]
fn header_is_six_bytes() {
    assert_eq!(MailboxHeader::PACKED_LEN, 6);
}

#[test]
fn header_roundtrip_and_layout() {
    let header = MailboxHeader {
        length: 0x0140,
        priority: Priority::Lowest,
        mailbox_type: MailboxType::VendorSpecific,
        counter: 5,
    };

    let mut buf = [0u8; 6];
    header.pack_to_slice(&mut buf).unwrap();

    // length little-endian in bytes 0..2, address bytes 2..4 zero.
    assert_eq!(&buf[0..4], &[0x40, 0x01, 0x00, 0x00]);
    // byte 5: type 0x0f in low nibble, counter 5 in bits 4..7 → 0x5f.
    assert_eq!(buf[5], 0x5f);

    assert_eq!(MailboxHeader::unpack_from_slice(&buf).unwrap(), header);
}

#[test]
fn build_then_parse_is_identity() {
    let data = [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03];
    let mut window = [0u8; 64]; // oversized window, as the ESC SM would be

    let n = build_frame(3, MailboxType::VendorSpecific, &data, &mut window).unwrap();
    assert_eq!(n, 6 + data.len());

    let msg = parse_frame(&window).unwrap();
    assert_eq!(msg.header.counter, 3);
    assert_eq!(msg.header.mailbox_type, MailboxType::VendorSpecific);
    assert_eq!(msg.header.length as usize, data.len());
    assert_eq!(&msg.body[..], &data[..]);
}

#[test]
fn parse_ignores_window_padding_past_declared_length() {
    // A short body in a big window: trailing window bytes are not part of the message.
    let data = [0xaa, 0xbb];
    let mut window = [0xffu8; 32];
    build_frame(1, MailboxType::VendorSpecific, &data, &mut window).unwrap();

    let msg = parse_frame(&window).unwrap();
    assert_eq!(&msg.body[..], &data[..]);
}

#[test]
fn build_rejects_oversized_payload() {
    // Body that would push the frame past MAILBOX_MAX_LEN.
    let data = [0u8; MAILBOX_MAX_LEN];
    let mut window = [0u8; MAILBOX_MAX_LEN + 16];
    assert!(build_frame(1, MailboxType::VendorSpecific, &data, &mut window).is_err());
}

#[test]
fn build_rejects_frame_larger_than_window() {
    // Body fits MAILBOX_MAX_LEN but not the (smaller) runtime window.
    let data = [0u8; 300];
    let mut window = [0u8; 256];
    assert!(build_frame(1, MailboxType::VendorSpecific, &data, &mut window).is_err());
}
