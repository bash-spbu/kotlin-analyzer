//! JDWP's data on the wire: big-endian numbers, strings as a length and
//! UTF-8, ids as wide as the VM says (`IdSizes`), locations, values.

use crate::{Error, IdSizes, Location, ObjectId, Tag, TypeTag, Value};

/// A command's data being written.
pub struct Writer<'sizes> {
    pub(crate) bytes: Vec<u8>,
    sizes: &'sizes IdSizes,
}

impl<'sizes> Writer<'sizes> {
    pub(crate) fn new(sizes: &'sizes IdSizes) -> Writer<'sizes> {
        Writer {
            bytes: Vec::new(),
            sizes,
        }
    }

    pub fn byte(&mut self, value: u8) -> &mut Self {
        self.bytes.push(value);
        self
    }

    pub fn boolean(&mut self, value: bool) -> &mut Self {
        self.byte(u8::from(value))
    }

    pub fn int(&mut self, value: i32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn long(&mut self, value: i64) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        self.int(i32::try_from(value.len()).unwrap_or(i32::MAX));
        self.bytes.extend_from_slice(value.as_bytes());
        self
    }

    fn id(&mut self, value: u64, size: usize) -> &mut Self {
        let bytes = value.to_be_bytes();
        self.bytes.extend_from_slice(&bytes[8 - size.min(8)..]);
        self
    }

    pub fn object(&mut self, value: ObjectId) -> &mut Self {
        self.id(value.0, self.sizes.object)
    }

    pub fn reference_type(&mut self, value: crate::ReferenceTypeId) -> &mut Self {
        self.id(value.0, self.sizes.reference_type)
    }

    pub fn method(&mut self, value: crate::MethodId) -> &mut Self {
        self.id(value.0, self.sizes.method)
    }

    pub fn field(&mut self, value: crate::FieldId) -> &mut Self {
        self.id(value.0, self.sizes.field)
    }

    pub fn frame(&mut self, value: crate::FrameId) -> &mut Self {
        self.id(value.0, self.sizes.frame)
    }

    pub fn location(&mut self, location: &Location) -> &mut Self {
        self.byte(location.type_tag as u8)
            .reference_type(location.class)
            .method(location.method)
            .long(location.index as i64)
    }
}

/// A reply's or an event's data being read.
pub struct Reader<'data> {
    data: &'data [u8],
    position: usize,
    sizes: IdSizes,
}

impl<'data> Reader<'data> {
    pub(crate) fn new(data: &'data [u8], sizes: IdSizes) -> Reader<'data> {
        Reader {
            data,
            position: 0,
            sizes,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'data [u8], Error> {
        let end = self.position + count;
        let bytes = self.data.get(self.position..end).ok_or(Error::Malformed)?;
        self.position = end;
        Ok(bytes)
    }

    pub fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn boolean(&mut self) -> Result<bool, Error> {
        Ok(self.byte()? != 0)
    }

    pub fn short(&mut self) -> Result<i16, Error> {
        let bytes = self.take(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn int(&mut self) -> Result<i32, Error> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes(
            bytes.try_into().map_err(|_| Error::Malformed)?,
        ))
    }

    pub fn long(&mut self) -> Result<i64, Error> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes(
            bytes.try_into().map_err(|_| Error::Malformed)?,
        ))
    }

    /// A count that is never negative.
    pub fn count(&mut self) -> Result<usize, Error> {
        usize::try_from(self.int()?).map_err(|_| Error::Malformed)
    }

    pub fn string(&mut self) -> Result<String, Error> {
        let length = self.count()?;
        let bytes = self.take(length)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn id(&mut self, size: usize) -> Result<u64, Error> {
        let bytes = self.take(size)?;
        Ok(bytes
            .iter()
            .fold(0u64, |id, byte| (id << 8) | u64::from(*byte)))
    }

    pub fn object(&mut self) -> Result<ObjectId, Error> {
        self.id(self.sizes.object).map(ObjectId)
    }

    pub fn reference_type(&mut self) -> Result<crate::ReferenceTypeId, Error> {
        self.id(self.sizes.reference_type)
            .map(crate::ReferenceTypeId)
    }

    pub fn method(&mut self) -> Result<crate::MethodId, Error> {
        self.id(self.sizes.method).map(crate::MethodId)
    }

    pub fn field(&mut self) -> Result<crate::FieldId, Error> {
        self.id(self.sizes.field).map(crate::FieldId)
    }

    pub fn frame(&mut self) -> Result<crate::FrameId, Error> {
        self.id(self.sizes.frame).map(crate::FrameId)
    }

    pub fn type_tag(&mut self) -> Result<TypeTag, Error> {
        TypeTag::of(self.byte()?)
    }

    pub fn location(&mut self) -> Result<Location, Error> {
        Ok(Location {
            type_tag: self.type_tag().unwrap_or(TypeTag::Class),
            class: self.reference_type()?,
            method: self.method()?,
            index: self.long()? as u64,
        })
    }

    /// A value that says its type (`tagged`).
    pub fn value(&mut self) -> Result<Value, Error> {
        let tag = Tag::of(self.byte()?)?;
        self.untagged_value(tag)
    }

    /// A value of a type known beforehand (a primitive array's element).
    pub fn untagged_value(&mut self, tag: Tag) -> Result<Value, Error> {
        Ok(match tag {
            Tag::Byte => Value::Byte(self.byte()? as i8),
            Tag::Boolean => Value::Boolean(self.boolean()?),
            Tag::Char => Value::Char(self.short()? as u16),
            Tag::Short => Value::Short(self.short()?),
            Tag::Int => Value::Int(self.int()?),
            Tag::Long => Value::Long(self.long()?),
            Tag::Float => Value::Float(f32::from_bits(self.int()? as u32)),
            Tag::Double => Value::Double(f64::from_bits(self.long()? as u64)),
            Tag::Void => Value::Void,
            object_tag => {
                let id = self.object()?;
                if id.0 == 0 {
                    Value::Null
                } else {
                    Value::Object {
                        tag: object_tag,
                        id,
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: IdSizes = IdSizes {
        field: 8,
        method: 8,
        object: 8,
        reference_type: 8,
        frame: 8,
    };

    #[test]
    fn what_is_written_reads_back() {
        let mut writer = Writer::new(&SIZES);
        let location = Location {
            type_tag: TypeTag::Class,
            class: crate::ReferenceTypeId(0x1234),
            method: crate::MethodId(7),
            index: 42,
        };
        writer
            .byte(3)
            .int(-2)
            .long(1 << 40)
            .string("Order.kt — é")
            .object(ObjectId(99))
            .location(&location);
        let bytes = writer.bytes.clone();
        let mut reader = Reader::new(&bytes, SIZES);
        assert_eq!(reader.byte().unwrap(), 3);
        assert_eq!(reader.int().unwrap(), -2);
        assert_eq!(reader.long().unwrap(), 1 << 40);
        assert_eq!(reader.string().unwrap(), "Order.kt — é");
        assert_eq!(reader.object().unwrap(), ObjectId(99));
        assert_eq!(reader.location().unwrap(), location);
        assert!(reader.byte().is_err(), "nothing more");
    }

    #[test]
    fn values_read_by_their_tag_and_narrow_ids_widen() {
        let narrow = IdSizes { object: 4, ..SIZES };
        let bytes = [
            b'I', 0, 0, 0, 42, // int 42
            b'Z', 1, // true
            b's', 0, 0, 0, 5, // a string object, 4-byte id
            b'L', 0, 0, 0, 0, // null
            b'D', 0x40, 0x09, 0x21, 0xfb, 0x54, 0x44, 0x2d, 0x18, // pi
        ];
        let mut reader = Reader::new(&bytes, narrow);
        assert_eq!(reader.value().unwrap(), Value::Int(42));
        assert_eq!(reader.value().unwrap(), Value::Boolean(true));
        assert_eq!(
            reader.value().unwrap(),
            Value::Object {
                tag: Tag::String,
                id: ObjectId(5)
            }
        );
        assert_eq!(reader.value().unwrap(), Value::Null);
        assert_eq!(reader.value().unwrap(), Value::Double(std::f64::consts::PI));
    }
}
