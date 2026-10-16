//! Captive-portal DNS answers (pure, so it can be tested on a host).

/// Builds the answer to a DNS `query`: its first question, answered with `ip` for A/ANY queries
/// and with no records otherwise. `None` for anything that isn't a well-formed standard query.
pub(crate) fn reply(query: &[u8], out: &mut [u8], ip: [u8; 4]) -> Option<usize> {
    const HEADER: usize = 12;
    let header = query.get(..HEADER)?;
    let is_response = header[2] & 0x80 != 0;
    let opcode = (header[2] >> 3) & 0x0F;
    let qdcount = u16::from_be_bytes([header[4], header[5]]);
    if is_response || opcode != 0 || qdcount == 0 {
        return None;
    }

    // first question: labels up to the root, then type + class (queries never use pointers)
    let mut pos = HEADER;
    loop {
        let len = *query.get(pos)? as usize;
        if len == 0 {
            break;
        }
        if len > 63 || pos - HEADER + len + 1 > 255 {
            return None;
        }
        pos += 1 + len;
    }
    let question_end = pos + 5;
    let question = query.get(HEADER..question_end)?;
    let qtype = u16::from_be_bytes([question[question.len() - 4], question[question.len() - 3]]);
    let qclass = u16::from_be_bytes([question[question.len() - 2], question[question.len() - 1]]);
    let answer = qclass == 1 && (qtype == 1 || qtype == 255);

    let len = HEADER + question.len() + if answer { 16 } else { 0 };
    let out = out.get_mut(..len)?;
    out[..2].copy_from_slice(&header[..2]); // id
    out[2] = 0x84 | (header[2] & 0x01); // response, authoritative, copy RD
    out[3] = 0;
    out[4..12].copy_from_slice(&[0, 1, 0, answer as u8, 0, 0, 0, 0]);
    out[HEADER..HEADER + question.len()].copy_from_slice(question);
    if answer {
        // name = pointer to the question, A, IN, ttl 60 s, 4-byte address
        let a = &mut out[HEADER + question.len()..];
        a[..12].copy_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        a[12..16].copy_from_slice(&ip);
    }
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::reply;

    const IP: [u8; 4] = [192, 168, 4, 1];

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&[0, 1]);
        q
    }

    #[test]
    fn answers_a_with_ap_ip() {
        let q = query("connectivitycheck.gstatic.com", 1);
        let mut out = [0; 512];
        let n = reply(&q, &mut out, IP).unwrap();
        assert_eq!(&out[..2], &[0x12, 0x34]);
        assert_eq!(out[2] & 0x80, 0x80);
        assert_eq!(&out[6..8], &[0, 1]); // one answer
        assert_eq!(&out[12..q.len()], &q[12..]);
        assert_eq!(&out[n - 4..n], &IP);
        assert_eq!(n, q.len() + 16);
    }

    #[test]
    fn aaaa_gets_empty_answer() {
        let q = query("captive.apple.com", 28);
        let mut out = [0; 512];
        let n = reply(&q, &mut out, IP).unwrap();
        assert_eq!(&out[6..8], &[0, 0]);
        assert_eq!(n, q.len());
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        let mut out = [0; 512];
        assert!(reply(&[], &mut out, IP).is_none());
        let mut resp = query("a.b", 1);
        resp[2] |= 0x80;
        assert!(reply(&resp, &mut out, IP).is_none());
        let mut seed = 0x1234_5678u32;
        for len in 0..600 {
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            _ = reply(&buf, &mut out, IP);
            _ = reply(&buf, &mut [0; 16], IP);
        }
    }
}
