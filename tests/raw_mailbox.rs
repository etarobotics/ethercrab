//! Round-trip + wire-layout tests for the raw ETG.1000.4 mailbox frame codec.
//!
//! These live as an integration test (not a `#[cfg(test)]` module) so they compile against the
//! library alone — the crate's internal unit tests `include_bytes!` EEPROM fixtures that the
//! published package excludes and so do not build in a vendored checkout.

use ethercrab::raw_mailbox::parse_frame;
use ethercrab::{MailboxFrame, MailboxHeader, MailboxType, Priority};
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
fn frame_packs_header_then_body() {
    let data = [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03];
    let frame = MailboxFrame {
        header: MailboxHeader {
            length: data.len() as u16,
            priority: Priority::Lowest,
            mailbox_type: MailboxType::Foe,
            counter: 3,
        },
        body: &data,
    };

    assert_eq!(frame.packed_len(), 6 + data.len());

    let mut window = [0u8; 64]; // oversized window, as the ESC SM would be
    let packed = frame.pack_to_slice(&mut window).unwrap();
    assert_eq!(packed.len(), 6 + data.len());

    // The packed frame round-trips through the parser.
    let (header, body) = parse_frame(&window).unwrap();
    assert_eq!(header.counter, 3);
    assert_eq!(header.mailbox_type, MailboxType::Foe);
    assert_eq!(header.length as usize, data.len());
    assert_eq!(body, &data[..]);
}

#[test]
fn parse_ignores_window_padding_past_declared_length() {
    // A short body in a big window: trailing window bytes are not part of the message.
    let data = [0xaa, 0xbb];
    let frame = MailboxFrame {
        header: MailboxHeader {
            length: data.len() as u16,
            priority: Priority::Lowest,
            mailbox_type: MailboxType::VendorSpecific,
            counter: 1,
        },
        body: &data,
    };

    let mut window = [0xffu8; 32];
    frame.pack_to_slice(&mut window).unwrap();

    let (_header, body) = parse_frame(&window).unwrap();
    assert_eq!(body, &data[..]);
}

#[test]
fn parse_rejects_window_shorter_than_declared_body() {
    // Header claims a 100-byte body but only 8 bytes of window are present.
    let header = MailboxHeader {
        length: 100,
        priority: Priority::Lowest,
        mailbox_type: MailboxType::Foe,
        counter: 1,
    };
    let mut window = [0u8; 8];
    header.pack_to_slice(&mut window[..6]).unwrap();

    assert!(parse_frame(&window).is_err());
}

#[test]
fn frame_pack_rejects_buffer_too_short_for_frame() {
    let data = [0u8; 60];
    let frame = MailboxFrame {
        header: MailboxHeader {
            length: data.len() as u16,
            priority: Priority::Lowest,
            mailbox_type: MailboxType::Foe,
            counter: 1,
        },
        body: &data,
    };

    // Buffer smaller than header + body must fail rather than truncate.
    let mut too_small = [0u8; 32];
    assert!(frame.pack_to_slice(&mut too_small).is_err());
}
