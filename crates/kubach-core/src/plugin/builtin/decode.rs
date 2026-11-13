//! Decoding a [`DynamicObject`] into a typed object.
//!
//! The typed object is read from the fields of the dynamic one where they
//! are: only what it keeps is copied. Going through a JSON value of the
//! whole object first (`to_value` → `from_value`) copies everything twice
//! and was most of the cost of a table row (#165, `docs/perf.md`).

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::DynamicObject;
use serde::de::value::BorrowedStrDeserializer;
use serde::de::{DeserializeOwned, DeserializeSeed, Deserializer, MapAccess, Visitor};
use serde_json::{Error, Value};

use crate::plugin::PluginError;

/// Decodes a dynamic object into a typed one.
pub(crate) fn decode<K: DeserializeOwned>(obj: &DynamicObject) -> Option<K> {
    K::deserialize(Object(obj)).ok()
}

pub(crate) fn decode_or_err<K: DeserializeOwned>(obj: &DynamicObject) -> Result<K, PluginError> {
    K::deserialize(Object(obj)).map_err(|e| PluginError::InvalidObject(e.to_string()))
}

/// The JSON object a [`DynamicObject`] serializes into, read in place:
/// `apiVersion`, `kind`, `metadata` and the fields of `data`.
struct Object<'de>(&'de DynamicObject);

impl<'de> Object<'de> {
    /// The fields of `data`. A missing `data` (`Null`) has none; anything
    /// but an object does not serialize, so it does not decode either.
    fn data(&self) -> Result<Option<&'de serde_json::Map<String, Value>>, Error> {
        match &self.0.data {
            Value::Object(map) => Ok(Some(map)),
            Value::Null => Ok(None),
            _ => Err(serde::de::Error::custom(
                "can only flatten structs and maps",
            )),
        }
    }

    /// For what is not read as a map (no typed object is): the JSON value
    /// of the whole object, as before.
    fn value(&self) -> Result<Value, Error> {
        serde_json::to_value(self.0)
    }
}

/// Forwards to the JSON value of the whole object.
macro_rules! through_value {
    ($($method:ident)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            self.value()?.$method(visitor)
        }
    )*};
}

impl<'de> Deserializer<'de> for Object<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let data = self.data()?;
        let object = self.0;
        // A field of `data` with the name of one of ours comes later in
        // the JSON object and replaces it.
        let ours = |name: &str| !data.is_some_and(|data| data.contains_key(name));
        let types = object.types.as_ref();
        let head = [
            types
                .filter(|_| ours("apiVersion"))
                .map(|types| ("apiVersion", Field::Text(&types.api_version))),
            types
                .filter(|_| ours("kind"))
                .map(|types| ("kind", Field::Text(&types.kind))),
            ours("metadata").then_some(("metadata", Field::Metadata(&object.metadata))),
        ];
        visitor.visit_map(Fields {
            head: head.into_iter(),
            data: data.map(|data| data.iter()),
            value: None,
        })
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.value()?.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        self.value()?.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.value()?.deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.value()?.deserialize_enum(name, variants, visitor)
    }

    through_value! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_u128 deserialize_f32 deserialize_f64 deserialize_char deserialize_str
        deserialize_string deserialize_bytes deserialize_byte_buf deserialize_unit
        deserialize_seq deserialize_identifier deserialize_ignored_any
    }
}

/// A field of the object outside `data`.
enum Field<'de> {
    Text(&'de str),
    Metadata(&'de ObjectMeta),
}

struct Fields<'de> {
    head: std::array::IntoIter<Option<(&'static str, Field<'de>)>, 3>,
    data: Option<serde_json::map::Iter<'de>>,
    /// The value of the key handed out last.
    value: Option<FieldValue<'de>>,
}

enum FieldValue<'de> {
    Ours(Field<'de>),
    Data(&'de Value),
}

impl<'de> MapAccess<'de> for Fields<'de> {
    type Error = Error;

    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Error> {
        let (key, value) = match self.head.by_ref().flatten().next() {
            Some((key, field)) => (key, FieldValue::Ours(field)),
            None => match self.data.as_mut().and_then(Iterator::next) {
                Some((key, value)) => (key.as_str(), FieldValue::Data(value)),
                None => return Ok(None),
            },
        };
        self.value = Some(value);
        seed.deserialize(BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, Error> {
        match self.value.take() {
            Some(FieldValue::Data(value)) => seed.deserialize(value),
            // As a JSON string, which also reads as an `Option`; the typed
            // object keeps the copy.
            Some(FieldValue::Ours(Field::Text(text))) => {
                seed.deserialize(Value::String(text.to_owned()))
            }
            // Small beside `data` once `managedFields` are dropped (#182).
            Some(FieldValue::Ours(Field::Metadata(metadata))) => {
                seed.deserialize(serde_json::to_value(metadata)?)
            }
            None => Err(serde::de::Error::custom("value is missing")),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::apps::v1::Deployment;
    use k8s_openapi::api::core::v1::{ConfigMap, Pod, Service};
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::decode;
    use crate::perf::{self, ClusterSize, PodShape};

    /// How objects were decoded before: through the JSON value of the
    /// whole object.
    fn through_value<K: DeserializeOwned>(obj: &DynamicObject) -> Option<K> {
        serde_json::to_value(obj)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
    }

    fn same<K: DeserializeOwned + PartialEq + std::fmt::Debug>(obj: &DynamicObject) {
        let error = super::decode_or_err::<K>(obj).err();
        assert_eq!(decode::<K>(obj), through_value::<K>(obj), "{error:?}");
        assert_eq!(error.is_some(), through_value::<K>(obj).is_none());
    }

    fn same_as_every_type(obj: &DynamicObject) {
        same::<Pod>(obj);
        same::<Deployment>(obj);
        same::<Service>(obj);
        same::<ConfigMap>(obj);
        same::<Value>(obj);
        same::<Option<Pod>>(obj);
        same::<DynamicObject2>(obj);
        // Not what a typed object is read as.
        same::<String>(obj);
        same::<Vec<Value>>(obj);
        same::<()>(obj);
    }

    /// `DynamicObject` has no `PartialEq`.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct DynamicObject2 {
        #[serde(rename = "apiVersion")]
        api_version: Option<String>,
        kind: Option<String>,
        metadata: Value,
        #[serde(flatten)]
        data: Value,
    }

    #[test]
    fn typed_objects_are_what_the_json_of_the_object_decodes_into() {
        for shape in [PodShape::FULL, PodShape::MANAGED, PodShape::SLIM] {
            for pod in perf::pods(40, 3, shape) {
                same_as_every_type(&pod);
                assert!(decode::<Pod>(&pod).is_some_and(|pod| pod.status.is_some()));
            }
        }
        let size = ClusterSize {
            namespaces: 2,
            apps: 3,
            replicas: 2,
        };
        for object in perf::cluster(size, PodShape::SLIM) {
            same_as_every_type(&object);
        }
    }

    #[test]
    fn odd_objects_decode_as_before() {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Pod"));
        // No `data` at all, and no type.
        let mut bare = DynamicObject::new("bare", &resource).within("web");
        same_as_every_type(&bare);
        assert!(decode::<Pod>(&bare).is_some());
        bare.types = None;
        same_as_every_type(&bare);
        // `data` naming our own fields replaces them.
        let mut shadowed = DynamicObject::new("outer", &resource);
        shadowed.data = json!({
            "kind": "Pod",
            "metadata": { "name": "inner" },
            "spec": { "nodeName": "n1" }
        });
        same_as_every_type(&shadowed);
        let pod = decode::<Pod>(&shadowed).unwrap();
        assert_eq!(pod.metadata.name.as_deref(), Some("inner"));
        // Fields of the wrong type fail both ways.
        let mut wrong = DynamicObject::new("wrong", &resource);
        wrong.data = json!({ "spec": { "containers": "none" } });
        same_as_every_type(&wrong);
        assert!(decode::<Pod>(&wrong).is_none());
        // `data` that is no object does not serialize.
        for data in [json!("text"), json!(3), json!([1, 2]), json!(true)] {
            let mut object = DynamicObject::new("scalar", &resource);
            object.data = data;
            same_as_every_type(&object);
            assert!(decode::<Pod>(&object).is_none());
            assert!(decode::<Value>(&object).is_none());
        }
    }
}
