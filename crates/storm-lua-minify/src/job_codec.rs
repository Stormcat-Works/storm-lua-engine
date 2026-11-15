//! Minimal self-owned binary codec for candidate-search worker transfers.
//!
//! This replaces the former `bincode` dependency (declared unmaintained in
//! RUSTSEC-2025-0141). The encoded values are transient: candidate jobs and
//! search contexts cross the compiler-wasm Worker boundary inside one
//! session, are never persisted, and need no cross-version compatibility, so
//! the format is a compact private layout driven by serde's data model:
//!
//! - `bool`: one byte (`0` or `1`)
//! - unsigned integers: unsigned LEB128
//! - signed integers: zigzag, then unsigned LEB128
//! - `f32` / `f64`: little-endian bit pattern
//! - `char`: Unicode scalar value as unsigned LEB128
//! - strings, byte buffers, sequences and maps: LEB128 length, then elements
//! - `Option`: one tag byte (`0` = none, `1` = some), then the payload
//! - enums: LEB128 variant index, then the variant payload
//! - structs, tuples and tuple structs: fields in declaration order, with no
//!   names or counts (the shape is fixed by the type)
//!
//! Anything outside this model (`deserialize_any`, identifiers, unknown
//! lengths) is an error, mirroring the contract the previous format had:
//! types that need human-readable branching key off
//! `is_human_readable() == false`, exactly as they did before.

use serde::de::{
    self, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess,
    Visitor,
};
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt::Display;

#[derive(Debug)]
pub(crate) struct CodecError(String);

impl Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

impl ser::Error for CodecError {
    fn custom<T: Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

impl de::Error for CodecError {
    fn custom<T: Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

type Result<T> = std::result::Result<T, CodecError>;

fn err<T>(msg: &str) -> Result<T> {
    Err(CodecError(msg.to_owned()))
}

/// Encode one value into the worker-transfer byte format.
pub(crate) fn encode<T: Serialize>(value: &T) -> std::result::Result<Vec<u8>, String> {
    let mut writer = Writer { out: Vec::new() };
    value
        .serialize(&mut writer)
        .map_err(|error: CodecError| error.to_string())?;
    Ok(writer.out)
}

/// Decode one value, rejecting trailing bytes.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> std::result::Result<T, String> {
    let mut deserializer = Deserializer {
        input: bytes,
        pos: 0,
    };
    let value = T::deserialize(&mut deserializer).map_err(|error: CodecError| error.to_string())?;
    if deserializer.pos != bytes.len() {
        return Err(format!(
            "trailing bytes after value: {} unconsumed",
            bytes.len() - deserializer.pos
        ));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------

struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn write_uleb128(&mut self, mut value: u128) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn write_len(&mut self, len: usize) {
        self.write_uleb128(len as u128);
    }
}

fn zigzag(value: i128) -> u128 {
    ((value << 1) ^ (value >> 127)) as u128
}

impl<'a> Serializer for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, value: bool) -> Result<()> {
        self.out.push(u8::from(value));
        Ok(())
    }

    fn serialize_i8(self, value: i8) -> Result<()> {
        self.serialize_i128(i128::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<()> {
        self.serialize_i128(i128::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<()> {
        self.serialize_i128(i128::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<()> {
        self.serialize_i128(i128::from(value))
    }

    fn serialize_i128(self, value: i128) -> Result<()> {
        self.write_uleb128(zigzag(value));
        Ok(())
    }

    fn serialize_u8(self, value: u8) -> Result<()> {
        self.serialize_u128(u128::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<()> {
        self.serialize_u128(u128::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<()> {
        self.serialize_u128(u128::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<()> {
        self.serialize_u128(u128::from(value))
    }

    fn serialize_u128(self, value: u128) -> Result<()> {
        self.write_uleb128(value);
        Ok(())
    }

    fn serialize_f32(self, value: f32) -> Result<()> {
        self.out.extend_from_slice(&value.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_f64(self, value: f64) -> Result<()> {
        self.out.extend_from_slice(&value.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_char(self, value: char) -> Result<()> {
        self.write_uleb128(u128::from(value as u32));
        Ok(())
    }

    fn serialize_str(self, value: &str) -> Result<()> {
        self.serialize_bytes(value.as_bytes())
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<()> {
        self.write_len(value.len());
        self.out.extend_from_slice(value);
        Ok(())
    }

    fn serialize_none(self) -> Result<()> {
        self.out.push(0);
        Ok(())
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<()> {
        self.out.push(1);
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<()> {
        self.write_uleb128(u128::from(variant_index));
        Ok(())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<()> {
        self.write_uleb128(u128::from(variant_index));
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
        match len {
            Some(len) => {
                self.write_len(len);
                Ok(self)
            }
            None => err("job codec requires sequence lengths"),
        }
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        self.write_uleb128(u128::from(variant_index));
        Ok(self)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
        match len {
            Some(len) => {
                self.write_len(len);
                Ok(self)
            }
            None => err("job codec requires map lengths"),
        }
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        self.write_uleb128(u128::from(variant_index));
        Ok(self)
    }
}

impl<'a> SerializeSeq for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeTuple for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeTupleStruct for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeTupleVariant for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeMap for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        key.serialize(&mut **self)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeStruct for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> SerializeStructVariant for &'a mut Writer {
    type Ok = ();
    type Error = CodecError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Deserializer
// ---------------------------------------------------------------------------

struct Deserializer<'de> {
    input: &'de [u8],
    pos: usize,
}

impl<'de> Deserializer<'de> {
    fn read_byte(&mut self) -> Result<u8> {
        match self.input.get(self.pos) {
            Some(byte) => {
                self.pos += 1;
                Ok(*byte)
            }
            None => err("unexpected end of input"),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'de [u8]> {
        let end = match self.pos.checked_add(count) {
            Some(end) if end <= self.input.len() => end,
            _ => return err("unexpected end of input"),
        };
        let slice = &self.input[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_uleb128(&mut self) -> Result<u128> {
        let mut value: u128 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            if shift >= 128 {
                return err("variable-length integer is too large");
            }
            value |= u128::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn read_len(&mut self) -> Result<usize> {
        let value = self.read_uleb128()?;
        usize::try_from(value).map_err(|_| CodecError("length does not fit usize".to_owned()))
    }

    fn read_signed(&mut self) -> Result<i128> {
        let raw = self.read_uleb128()?;
        Ok(((raw >> 1) as i128) ^ -((raw & 1) as i128))
    }

    fn read_bytes(&mut self) -> Result<&'de [u8]> {
        let len = self.read_len()?;
        self.take(len)
    }
}

macro_rules! impl_deserialize_int {
    ($method:ident, $visit:ident, $ty:ty, signed) => {
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let raw = self.read_signed()?;
            let value = <$ty>::try_from(raw).map_err(|_| {
                CodecError(concat!("integer out of range for ", stringify!($ty)).to_owned())
            })?;
            visitor.$visit(value)
        }
    };
    ($method:ident, $visit:ident, $ty:ty, unsigned) => {
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let raw = self.read_uleb128()?;
            let value = <$ty>::try_from(raw).map_err(|_| {
                CodecError(concat!("integer out of range for ", stringify!($ty)).to_owned())
            })?;
            visitor.$visit(value)
        }
    };
}

impl<'de> serde::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = CodecError;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        err("job codec does not support deserialize_any")
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        err("job codec does not support deserialize_ignored_any")
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        err("job codec does not encode identifiers")
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.read_byte()? {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            _ => err("invalid bool byte"),
        }
    }

    impl_deserialize_int!(deserialize_i8, visit_i8, i8, signed);
    impl_deserialize_int!(deserialize_i16, visit_i16, i16, signed);
    impl_deserialize_int!(deserialize_i32, visit_i32, i32, signed);
    impl_deserialize_int!(deserialize_i64, visit_i64, i64, signed);
    impl_deserialize_int!(deserialize_i128, visit_i128, i128, signed);
    impl_deserialize_int!(deserialize_u8, visit_u8, u8, unsigned);
    impl_deserialize_int!(deserialize_u16, visit_u16, u16, unsigned);
    impl_deserialize_int!(deserialize_u32, visit_u32, u32, unsigned);
    impl_deserialize_int!(deserialize_u64, visit_u64, u64, unsigned);
    impl_deserialize_int!(deserialize_u128, visit_u128, u128, unsigned);

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bytes = self.take(4)?;
        let mut array = [0_u8; 4];
        array.copy_from_slice(bytes);
        visitor.visit_f32(f32::from_bits(u32::from_le_bytes(array)))
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        visitor.visit_f64(f64::from_bits(u64::from_le_bytes(array)))
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let raw = self.read_uleb128()?;
        let value = u32::try_from(raw)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| CodecError("invalid char value".to_owned()))?;
        visitor.visit_char(value)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bytes = self.read_bytes()?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CodecError("invalid UTF-8 in string".to_owned()))?;
        visitor.visit_borrowed_str(text)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bytes = self.read_bytes()?;
        visitor.visit_borrowed_bytes(bytes)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bytes = self.read_bytes()?;
        visitor.visit_byte_buf(bytes.to_vec())
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.read_byte()? {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(self),
            _ => err("invalid option tag"),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.read_len()?;
        visitor.visit_seq(SeqReader {
            de: self,
            remaining: len,
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(SeqReader {
            de: self,
            remaining: len,
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_seq(SeqReader {
            de: self,
            remaining: len,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_seq(SeqReader {
            de: self,
            remaining: fields.len(),
        })
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.read_len()?;
        visitor.visit_map(MapReader {
            de: self,
            remaining: len,
        })
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let raw = self.read_uleb128()?;
        let index = u32::try_from(raw)
            .map_err(|_| CodecError("enum variant index does not fit u32".to_owned()))?;
        visitor.visit_enum(EnumReader { de: self, index })
    }
}

struct SeqReader<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    remaining: usize,
}

impl<'de> SeqAccess<'de> for SeqReader<'_, 'de> {
    type Error = CodecError;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

struct MapReader<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    remaining: usize,
}

impl<'de> MapAccess<'de> for MapReader<'_, 'de> {
    type Error = CodecError;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V2: DeserializeSeed<'de>>(&mut self, seed: V2) -> Result<V2::Value> {
        self.remaining -= 1;
        seed.deserialize(&mut *self.de)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

struct EnumReader<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    index: u32,
}

impl<'a, 'de> EnumAccess<'de> for EnumReader<'a, 'de> {
    type Error = CodecError;
    type Variant = VariantReader<'a, 'de>;

    fn variant_seed<V2: DeserializeSeed<'de>>(
        self,
        seed: V2,
    ) -> Result<(V2::Value, Self::Variant)> {
        let value = seed.deserialize(de::value::U32Deserializer::<CodecError>::new(self.index))?;
        Ok((value, VariantReader { de: self.de }))
    }
}

struct VariantReader<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'de> VariantAccess<'de> for VariantReader<'_, 'de> {
    type Error = CodecError;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(&mut *self.de)
    }

    fn tuple_variant<V2: Visitor<'de>>(self, len: usize, visitor: V2) -> Result<V2::Value> {
        visitor.visit_seq(SeqReader {
            de: self.de,
            remaining: len,
        })
    }

    fn struct_variant<V2: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V2,
    ) -> Result<V2::Value> {
        visitor.visit_seq(SeqReader {
            de: self.de,
            remaining: fields.len(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{decode, encode};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum SampleEnum {
        Unit,
        Newtype(String),
        Tuple(u32, bool),
        Struct { count: usize, ratio: f64 },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        values: Vec<i64>,
        maybe: Option<SampleEnum>,
        pair: (u8, char),
        map: BTreeMap<String, f32>,
        raw: Vec<u8>,
        negative: i128,
        big: u128,
    }

    #[test]
    fn round_trip_covers_the_supported_data_model() {
        let sample = Sample {
            name: "雪😀".to_owned(),
            values: vec![0, -1, 63, -64, 123_456_789, i64::MIN],
            maybe: Some(SampleEnum::Struct {
                count: 42,
                ratio: -0.5,
            }),
            pair: (7, 'é'),
            map: BTreeMap::from([("a".to_owned(), 1.5_f32), ("b".to_owned(), -2.25_f32)]),
            raw: vec![0, 1, 2, 255],
            negative: -123_456_789_012_345_i128,
            big: u128::MAX - 1,
        };
        let bytes = encode(&sample).unwrap();
        assert_eq!(decode::<Sample>(&bytes).unwrap(), sample);
        for variant in [
            SampleEnum::Unit,
            SampleEnum::Newtype("x".to_owned()),
            SampleEnum::Tuple(9, true),
        ] {
            let bytes = encode(&variant).unwrap();
            assert_eq!(decode::<SampleEnum>(&bytes).unwrap(), variant);
        }
        let none: Option<u32> = None;
        assert_eq!(
            decode::<Option<u32>>(&encode(&none).unwrap()).unwrap(),
            none
        );
    }

    #[test]
    fn decode_rejects_trailing_and_truncated_input() {
        let mut bytes = encode(&42_u32).unwrap();
        bytes.push(0);
        assert!(decode::<u32>(&bytes).is_err());
        assert!(decode::<u64>(&[0x80]).is_err());
        assert!(decode::<bool>(&[2]).is_err());
    }
}
