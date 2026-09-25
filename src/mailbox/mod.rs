pub mod coe;

#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[cfg_attr(test, derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
/// Mailbox message priority (ETG.1000.4).
pub enum Priority {
    /// Lowest priority.
    #[default]
    Lowest = 0x00,
    /// Low priority.
    Low = 0x01,
    /// High priority.
    High = 0x02,
    /// Highest priority.
    Highest = 0x03,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ethercrab_wire::EtherCrabWireReadWrite)]
#[cfg_attr(test, derive(arbitrary::Arbitrary))]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
/// Mailbox protocol carried in the mailbox data (ETG.1000.4 mailbox type).
pub enum MailboxType {
    /// error (ERR)
    Err = 0x00,
    /// ADS over EtherCAT (AoE)
    Aoe = 0x01,
    /// Ethernet over EtherCAT (EoE)
    Eoe = 0x02,
    /// CAN application protocol over EtherCAT (CoE)
    Coe = 0x03,
    /// File Access over EtherCAT (FoE)
    Foe = 0x04,
    /// Servo profile over EtherCAT (SoE)
    Soe = 0x05,
    // 0x06 -0x0e: reserved
    /// Vendor specific
    VendorSpecific = 0x0f,
}
