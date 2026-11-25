//! Hand-written SHA-256 (FIPS 180-4).
//!
//! The plugin builds for `wasm32-wasip2` and is not allowed to pull a new crate
//! for this (see `AGENTS.md` "不为小功能引入大依赖"). The whole thing is ~60
//! lines of arithmetic, verified against the standard test vectors below.
//!
//! A hand-written hash is only trusted because it matches the published vectors
//! and the real engine jar's recorded digest (`engine/ENGINE_VERSION.sha256`,
//! cross-checked with `sha256sum` during T5-1). Do not change the rounds.

/// FIPS-180-4 round constants: the first 32 bits of the fractional parts of the
/// cube roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash state (fractional parts of the square roots of the first 8 primes).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = H0;

    // Padding: 0x80, zeros, then the 64-bit big-endian bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut tail = Vec::with_capacity(72);
    tail.push(0x80u8);
    while (data.len() + tail.len()) % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&bit_len.to_be_bytes());

    let mut block = [0u8; 64];
    let mut compress = |chunk: &[u8]| {
        block.copy_from_slice(chunk);
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    };

    // Whole blocks of the body, then the final block(s): the remainder of the
    // body (data.len() % 64 bytes) followed by the pad + length. The remainder
    // must be compressed — it is the common off-by-one that drops the last bytes.
    let whole = data.len() - data.len() % 64;
    for chunk in data[..whole].chunks_exact(64) {
        compress(chunk);
    }
    let mut finish = Vec::with_capacity(data.len() - whole + tail.len());
    finish.extend_from_slice(&data[whole..]);
    finish.extend_from_slice(&tail);
    debug_assert_eq!(finish.len() % 64, 0, "padding must produce whole blocks");
    for chunk in finish.chunks_exact(64) {
        compress(chunk);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        for byte in word.to_be_bytes() {
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    /// Published FIPS/NIST vectors — the only reason a hand-rolled hash is OK.
    #[test]
    fn matches_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// A 56-byte input exercises the case where the 0x80 byte forces an extra block.
    #[test]
    fn pads_across_a_block_boundary() {
        // Exactly 56 bytes: 0x80 + 7 zero bytes push the length into a second block.
        let data = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(data.len(), 56);
        assert_eq!(
            sha256_hex(data),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
    }

    /// Multi-block body (>64 bytes) still hashes correctly.
    #[test]
    fn hashes_multiple_blocks() {
        let data = vec![b'a'; 1000];
        // sha256sum of 1000 'a' bytes.
        assert_eq!(
            sha256_hex(&data),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }
}
