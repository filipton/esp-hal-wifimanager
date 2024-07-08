pub fn hash(buf: &[u8]) -> u64 {
    let mut tmp = 0;
    for b in buf {
        tmp ^= *b as u64;
        tmp <<= 1;
    }

    tmp
}
