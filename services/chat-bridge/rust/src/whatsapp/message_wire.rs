//! Persist typed protocol messages in the existing protobuf JSON journal format.
//! Field presence, bytes, decimal 64-bit values and named enums come from the
//! generated protocol types; this does not interpret message content.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use serde::{
    Serialize,
    ser::{self, SerializeMap, SerializeSeq, SerializeStruct},
};

pub fn message_json(message: &waproto::whatsapp::Message) -> IvyResult<Json> {
    message.serialize(Wire).map(Node::json).map_err(|_| {
        IvyError::new(
            "whatsapp_message_invalid",
            "The protocol message could not be encoded.",
        )
    })
}

enum Node {
    Absent,
    Value(Json),
    Byte(u8),
    List(Vec<Node>),
    Map(Vec<(String, Json)>),
    Union(&'static str, Box<Node>),
}
impl Node {
    fn json(self) -> Json {
        match self {
            Self::Absent => Json::Null,
            Self::Value(v) => v,
            Self::Byte(v) => Json::Number(v.into()),
            Self::List(items) => Json::Array(items.into_iter().map(Self::json).collect()),
            Self::Map(pairs) => Json::object(pairs),
            Self::Union(name, value) => Json::object([(camel(name), value.json())]),
        }
    }
}
fn buffer(bytes: &[u8]) -> Node {
    Node::Value(STANDARD.encode(bytes).into())
}
fn camel(name: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if result.is_empty() {
            result.extend(c.to_lowercase());
        } else if upper {
            result.extend(c.to_uppercase());
            upper = false;
        } else {
            result.push(c);
        }
    }
    result
}
#[derive(Debug)]
struct EncodeError;
impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid protocol value")
    }
}
impl std::error::Error for EncodeError {}
impl ser::Error for EncodeError {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}
type Result<T> = std::result::Result<T, EncodeError>;
struct Wire;
macro_rules! small_number {
    ($($method:ident($ty:ty)),*) => { $(
        fn $method(self, v: $ty) -> Result<Node> { Ok(Node::Value(Json::Number(v as f64))) }
    )* };
}
impl ser::Serializer for Wire {
    type Ok = Node;
    type Error = EncodeError;
    type SerializeSeq = Sequence;
    type SerializeTuple = ser::Impossible<Node, EncodeError>;
    type SerializeTupleStruct = ser::Impossible<Node, EncodeError>;
    type SerializeTupleVariant = ser::Impossible<Node, EncodeError>;
    type SerializeMap = Map;
    type SerializeStruct = Object;
    type SerializeStructVariant = ser::Impossible<Node, EncodeError>;
    fn serialize_bool(self, v: bool) -> Result<Node> {
        Ok(Node::Value(v.into()))
    }
    fn serialize_u8(self, v: u8) -> Result<Node> {
        Ok(Node::Byte(v))
    }
    small_number!(
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_u16(u16),
        serialize_u32(u32)
    );
    fn serialize_f32(self, v: f32) -> Result<Node> {
        self.serialize_f64(v as f64)
    }
    fn serialize_f64(self, v: f64) -> Result<Node> {
        Ok(Node::Value(if v.is_finite() {
            Json::Number(v)
        } else if v.is_nan() {
            "NaN".into()
        } else if v.is_sign_positive() {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }))
    }
    fn serialize_i64(self, v: i64) -> Result<Node> {
        Ok(Node::Value(v.to_string().into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Node> {
        Ok(Node::Value(v.to_string().into()))
    }
    fn serialize_i128(self, _: i128) -> Result<Node> {
        Err(EncodeError)
    }
    fn serialize_u128(self, _: u128) -> Result<Node> {
        Err(EncodeError)
    }
    fn serialize_char(self, v: char) -> Result<Node> {
        self.serialize_str(&v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<Node> {
        Ok(Node::Value(v.into()))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Node> {
        Ok(buffer(v))
    }
    fn serialize_none(self) -> Result<Node> {
        Ok(Node::Absent)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<Node> {
        match v.serialize(self)? {
            // Protocol repeated fields are never optional. An optional empty
            // sequence is a present, empty bytes field.
            Node::List(values) if values.is_empty() => Ok(buffer(&[])),
            value => Ok(value),
        }
    }
    fn serialize_unit(self) -> Result<Node> {
        Ok(Node::Absent)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Node> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, name: &'static str) -> Result<Node> {
        Ok(Node::Value(name.into()))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<Node> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        v: &T,
    ) -> Result<Node> {
        Ok(Node::Union(name, Box::new(v.serialize(self)?)))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Sequence> {
        Ok(Sequence(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple> {
        Err(EncodeError)
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Err(EncodeError)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(EncodeError)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Map> {
        Ok(Map {
            pairs: Vec::new(),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Object> {
        Ok(Object { fields: Vec::new() })
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(EncodeError)
    }
}
struct Sequence(Vec<Node>);
impl SerializeSeq for Sequence {
    type Ok = Node;
    type Error = EncodeError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let value = match value.serialize(Wire)? {
            // The only nested sequence in protobuf is repeated bytes.
            Node::List(items) if items.is_empty() => buffer(&[]),
            value => value,
        };
        self.0.push(value);
        Ok(())
    }
    fn end(self) -> Result<Node> {
        if !self.0.is_empty() && self.0.iter().all(|v| matches!(v, Node::Byte(_))) {
            let bytes: Vec<_> = self
                .0
                .into_iter()
                .map(|v| match v {
                    Node::Byte(v) => v,
                    _ => unreachable!(),
                })
                .collect();
            Ok(buffer(&bytes))
        } else {
            Ok(Node::List(self.0))
        }
    }
}
struct Object {
    fields: Vec<(String, Json)>,
}
impl SerializeStruct for Object {
    type Ok = Node;
    type Error = EncodeError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, v: &T) -> Result<()> {
        match v.serialize(Wire)? {
            Node::Absent => {}
            Node::List(ref values) if values.is_empty() => {}
            Node::Map(ref values) if values.is_empty() => {}
            Node::Union(name, value) => self.fields.push((camel(name), value.json())),
            value => self.fields.push((camel(name), value.json())),
        }
        Ok(())
    }
    fn end(self) -> Result<Node> {
        Ok(Node::Value(Json::object(self.fields)))
    }
}
struct Map {
    pairs: Vec<(String, Json)>,
    key: Option<String>,
}
impl SerializeMap for Map {
    type Ok = Node;
    type Error = EncodeError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.key = Some(match value.serialize(Wire)?.json() {
            Json::String(ref v) => v.to_utf8().map_err(|_| EncodeError)?,
            Json::Number(ref v) => v.to_string(),
            _ => return Err(EncodeError),
        });
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.pairs.push((
            self.key.take().ok_or(EncodeError)?,
            value.serialize(Wire)?.json(),
        ));
        Ok(())
    }
    fn end(self) -> Result<Node> {
        Ok(Node::Map(self.pairs))
    }
}
