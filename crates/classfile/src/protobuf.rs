//! The protobuf wire format, as far as reading Kotlin metadata needs:
//! fields of varints, fixed-width numbers and length-delimited bytes.

/// One field's value as the wire carries it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum WireValue<'bytes> {
    Varint(u64),
    Fixed64,
    Bytes(&'bytes [u8]),
    Fixed32,
}

impl<'bytes> WireValue<'bytes> {
    pub(crate) fn as_u64(self) -> Option<u64> {
        match self {
            WireValue::Varint(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_i32(self) -> Option<i32> {
        self.as_u64().map(|value| value as i32)
    }

    pub(crate) fn as_bytes(self) -> Option<&'bytes [u8]> {
        match self {
            WireValue::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// A repeated integer field arrives either packed (one byte run of
    /// varints) or as one varint per occurrence; both read the same.
    pub(crate) fn push_integers(self, into: &mut Vec<i32>) {
        match self {
            WireValue::Varint(value) => into.push(value as i32),
            WireValue::Bytes(bytes) => {
                let mut reader = Reader::new(bytes);
                while let Some(value) = reader.varint() {
                    into.push(value as i32);
                }
            }
            _ => {}
        }
    }
}

/// Reads fields from a message's bytes.
pub(crate) struct Reader<'bytes> {
    bytes: &'bytes [u8],
    position: usize,
}

impl<'bytes> Reader<'bytes> {
    pub(crate) fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.position)?;
            self.position += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, length: usize) -> Option<&'bytes [u8]> {
        let end = self.position.checked_add(length)?;
        let bytes = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(bytes)
    }

    /// A length-prefixed message (`writeDelimitedTo`).
    pub(crate) fn delimited(&mut self) -> Option<&'bytes [u8]> {
        let length = self.varint()? as usize;
        self.take(length)
    }

    /// What's left unread.
    pub(crate) fn rest(&self) -> &'bytes [u8] {
        &self.bytes[self.position.min(self.bytes.len())..]
    }

    /// The next field, or `None` at the end (or on malformed input).
    pub(crate) fn field(&mut self) -> Option<(u32, WireValue<'bytes>)> {
        if self.position >= self.bytes.len() {
            return None;
        }
        let key = self.varint()?;
        let number = (key >> 3) as u32;
        let value = match key & 0x7 {
            0 => WireValue::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                WireValue::Fixed64
            }
            2 => {
                let length = self.varint()? as usize;
                WireValue::Bytes(self.take(length)?)
            }
            5 => {
                self.take(4)?;
                WireValue::Fixed32
            }
            _ => return None,
        };
        Some((number, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_varints_bytes_and_packed_integers() {
        // field 1 = 150 (varint), field 2 = "hi", field 3 = packed [3, 270]
        let bytes = [
            0x08, 0x96, 0x01, 0x12, 0x02, b'h', b'i', 0x1a, 0x03, 0x03, 0x8e, 0x02,
        ];
        let mut reader = Reader::new(&bytes);
        let (number, value) = reader.field().unwrap();
        assert_eq!((number, value.as_u64()), (1, Some(150)));
        let (number, value) = reader.field().unwrap();
        assert_eq!((number, value.as_bytes()), (2, Some(&b"hi"[..])));
        let (number, value) = reader.field().unwrap();
        let mut integers = Vec::new();
        value.push_integers(&mut integers);
        assert_eq!((number, integers), (3, vec![3, 270]));
        assert!(reader.field().is_none());
    }
}
