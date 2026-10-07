//! Checked decoding of the CONSTANTS section. Integer payloads use little-endian
//! bytes; signed payloads preserve their two's-complement representation.

use std::io;

use nsbc::Constant;
use type_pool::TypeIndex;

pub(super) fn decode(data: &[u8]) -> io::Result<Vec<Constant>> {
    let mut reader = ConstantReader { remaining: data };
    let count = u32::from_le_bytes(reader.fixed()?) as usize;
    // Even an empty string or BigInt needs one tag and a four-byte length.
    // Check the count before reserving memory from untrusted archive input.
    if count > reader.remaining.len() / 5 {
        return Err(invalid("constant count exceeds CONSTANTS section data"));
    }
    let mut constants = Vec::new();
    constants
        .try_reserve_exact(count)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "cannot allocate constants"))?;
    for _ in 0..count {
        let [tag] = reader.fixed()?;
        let constant = match tag {
            0 => Constant::Int(i64::from_le_bytes(reader.fixed()?)),
            1 => Constant::UInt(u64::from_le_bytes(reader.fixed()?)),
            2 => Constant::Float(f64::from_le_bytes(reader.fixed()?)),
            3 => {
                let bytes = reader.variable()?;
                let value = std::str::from_utf8(bytes)
                    .map_err(|_| invalid("invalid UTF-8 in string constant"))?;
                Constant::Str(value.to_owned())
            }
            4 => Constant::BigInt(reader.variable()?.to_vec()),
            5 => Constant::Int128(i128::from_le_bytes(reader.fixed()?)),
            6 => Constant::UInt128(u128::from_le_bytes(reader.fixed()?)),
            7 => {
                let ty = TypeIndex::from_raw(u32::from_le_bytes(reader.fixed()?));
                if ty == TypeIndex::INVALID {
                    return Err(invalid("invalid type index in type constant"));
                }
                Constant::Type(ty)
            }
            8 => {
                let type_index = TypeIndex::from_raw(u32::from_le_bytes(reader.fixed()?));
                let variant = u32::from_le_bytes(reader.fixed()?);
                if type_index == TypeIndex::INVALID || variant >= (1 << 25) {
                    return Err(invalid("invalid enum constant representation"));
                }
                Constant::Enum {
                    type_index,
                    variant,
                }
            }
            9 => Constant::Char(
                char::from_u32(u32::from_le_bytes(reader.fixed()?))
                    .ok_or_else(|| invalid("invalid Unicode scalar in character constant"))?,
            ),
            _ => return Err(invalid("unknown constant tag")),
        };
        constants.push(constant);
    }
    if !reader.remaining.is_empty() {
        return Err(invalid("trailing bytes in CONSTANTS section"));
    }
    Ok(constants)
}

struct ConstantReader<'a> {
    remaining: &'a [u8],
}

impl<'a> ConstantReader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if length > self.remaining.len() {
            return Err(invalid("truncated constant payload"));
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }

    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    fn variable(&mut self) -> io::Result<&'a [u8]> {
        let length = u32::from_le_bytes(self.fixed()?) as usize;
        self.take(length)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::decode;
    use std::io::ErrorKind;

    #[test]
    fn enum_constants_reject_truncation_invalid_indices_and_oversized_tags() {
        for length in 0..8 {
            let mut bytes = vec![1, 0, 0, 0, 8];
            bytes.resize(5 + length, 0);
            assert!(decode(&bytes).is_err());
        }
        for (ty, tag) in [(u32::MAX, 0_u32), (0, 1 << 25)] {
            let mut bytes = vec![1, 0, 0, 0, 8];
            bytes.extend_from_slice(&ty.to_le_bytes());
            bytes.extend_from_slice(&tag.to_le_bytes());
            assert!(decode(&bytes).is_err());
        }
    }

    #[test]
    fn character_constants_reject_invalid_scalars_and_truncation() {
        for scalar in [0xd800_u32, 0xdfff, 0x110000, u32::MAX] {
            let mut bytes = vec![1, 0, 0, 0, 9];
            bytes.extend_from_slice(&scalar.to_le_bytes());
            let error = decode(&bytes).unwrap_err();
            assert!(error.to_string().contains("invalid Unicode scalar"));
        }
        for length in 0..4 {
            let mut bytes = vec![1, 0, 0, 0, 9];
            bytes.resize(5 + length, 0);
            assert!(decode(&bytes).is_err());
        }
        let error = decode(&[1, 0, 0, 0, 10, 0, 0, 0, 0]).unwrap_err();
        assert!(error.to_string().contains("unknown constant tag"));
    }

    #[test]
    fn rejects_malformed_constant_sections() {
        let mut cases = vec![
            vec![],
            vec![1, 0, 0, 0],
            vec![1, 0, 0, 0, 255, 0, 0, 0, 0],
            vec![1, 0, 0, 0, 3, 1, 0, 0, 0, 255],
            vec![1, 0, 0, 0, 4, 255, 255, 255, 255],
            vec![0, 0, 0, 0, 0],
            vec![255, 255, 255, 255, 0, 0, 0, 0, 0],
        ];
        for tag in [5, 6] {
            for payload_length in 0..16 {
                let mut bytes = vec![1, 0, 0, 0, tag];
                bytes.resize(bytes.len() + payload_length, 0);
                cases.push(bytes);
            }
        }
        for payload_length in 0..4 {
            let mut bytes = vec![1, 0, 0, 0, 7];
            bytes.resize(bytes.len() + payload_length, 0);
            cases.push(bytes);
        }
        cases.push(vec![1, 0, 0, 0, 7, 255, 255, 255, 255]);
        for bytes in cases {
            assert_eq!(decode(&bytes).unwrap_err().kind(), ErrorKind::InvalidData);
        }
    }
}
