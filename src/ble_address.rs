/// Static random BLE address from the device mac.
///
/// BLE addresses are little-endian, so the mac is reversed, and the top two bits of the last byte
/// are the address type: `11` is static. Other values make it a resolvable or non-resolvable
/// private address, or reserved (`10`), which scanners like CoreBluetooth drop.
pub(crate) fn static_random_address(mut mac: [u8; 6]) -> [u8; 6] {
    mac.reverse();
    mac[5] |= 0xc0;
    mac
}

#[cfg(test)]
mod tests {
    use super::static_random_address;

    #[test]
    fn reverses_mac_into_little_endian() {
        let address = static_random_address([0xd0, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(address, [0x55, 0x44, 0x33, 0x22, 0x11, 0xd0]);
    }

    #[test]
    fn always_static_type() {
        // first mac byte becomes the address' top byte; covers all four type bit patterns
        for first in [0x0a, 0x4a, 0x8a, 0xca] {
            let address = static_random_address([first, 0x11, 0x22, 0x33, 0x44, 0x55]);
            assert_eq!(address[5] >> 6, 0b11);
        }
    }
}
