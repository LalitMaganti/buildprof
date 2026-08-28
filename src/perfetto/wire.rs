//! Allocation-free protobuf wire encoder.

use std::io::{self, Write};

const VARINT: u64 = 0;
const LENGTH_DELIMITED: u64 = 2;
const FIELD_NUMBER_SHIFT: u32 = 3;
const VARINT_GROUP_BITS: u32 = 7;
const VARINT_CONTINUATION_BIT: u8 = 0x80;
const VARINT_PAYLOAD_MASK: u8 = 0x7f;
const MAX_VARINT_BYTES: usize = 10;

pub(super) struct Encoder<'a> {
    output: Output<'a>,
}

enum Output<'a> {
    Counter(usize),
    Writer(&'a mut dyn Write),
}

impl<'a> Encoder<'a> {
    pub(super) fn writer(output: &'a mut dyn Write) -> Self {
        Self {
            output: Output::Writer(output),
        }
    }

    fn counter() -> Self {
        Self {
            output: Output::Counter(0),
        }
    }

    pub(super) fn uint(&mut self, field: u32, value: u64) -> io::Result<()> {
        self.key(field, VARINT)?;
        self.varint(value)
    }

    pub(super) fn boolean(&mut self, field: u32, value: bool) -> io::Result<()> {
        self.uint(field, u64::from(value))
    }

    pub(super) fn string(&mut self, field: u32, value: &str) -> io::Result<()> {
        self.bytes(field, value.as_bytes())
    }

    pub(super) fn bytes(&mut self, field: u32, value: &[u8]) -> io::Result<()> {
        self.key(field, LENGTH_DELIMITED)?;
        self.varint(value.len() as u64)?;
        self.write(value)
    }

    pub(super) fn message(
        &mut self,
        field: u32,
        encode: &mut dyn FnMut(&mut Encoder<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut counter = Encoder::counter();
        encode(&mut counter)?;
        let Output::Counter(length) = counter.output else {
            unreachable!()
        };

        self.key(field, LENGTH_DELIMITED)?;
        self.varint(length as u64)?;
        if matches!(self.output, Output::Writer(_)) {
            encode(self)?;
        } else if let Output::Counter(total) = &mut self.output {
            *total += length;
        }
        Ok(())
    }

    fn key(&mut self, field: u32, wire_type: u64) -> io::Result<()> {
        self.varint((u64::from(field) << FIELD_NUMBER_SHIFT) | wire_type)
    }

    fn varint(&mut self, mut value: u64) -> io::Result<()> {
        let mut bytes = [0; MAX_VARINT_BYTES];
        let mut length = 0;
        while value >= u64::from(VARINT_CONTINUATION_BIT) {
            bytes[length] = (value as u8 & VARINT_PAYLOAD_MASK) | VARINT_CONTINUATION_BIT;
            value >>= VARINT_GROUP_BITS;
            length += 1;
        }
        bytes[length] = value as u8;
        self.write(&bytes[..=length])
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.output {
            Output::Counter(length) => {
                *length += bytes.len();
                Ok(())
            }
            Output::Writer(output) => output.write_all(bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_nested_messages_without_a_payload_buffer() {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::writer(&mut bytes);
        encoder
            .message(1, &mut |message| {
                message.uint(1, 150)?;
                message.string(2, "ok")
            })
            .unwrap();
        assert_eq!(bytes, [10, 7, 8, 150, 1, 18, 2, b'o', b'k']);
    }
}
