//! Custom `Deserialize` impl for [`PipelineContextData`].
//!
//! GitHub's protocol may serialize context data as a typed object (with
//! `type` discriminator) OR as a bare string/bool/number/null. The custom
//! visitor handles both forms uniformly.

use serde::Deserialize;
use serde::de;

use super::context_data::{DictEntry, PipelineContextData};

impl<'de> Deserialize<'de> for PipelineContextData {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
      type Value = PipelineContextData;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a PipelineContextData object, string, bool, number, or null")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::string(v.to_owned()))
      }

      fn visit_bool<E: de::Error>(self, v: bool) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::bool(v))
      }

      fn visit_i64<E: de::Error>(self, v: i64) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::number(v as f64))
      }

      fn visit_u64<E: de::Error>(self, v: u64) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::number(v as f64))
      }

      fn visit_f64<E: de::Error>(self, v: f64) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::number(v))
      }

      fn visit_unit<E: de::Error>(self) -> Result<PipelineContextData, E> {
        Ok(PipelineContextData::null())
      }

      fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<PipelineContextData, A::Error> {
        #[derive(Deserialize)]
        struct Inner {
          #[serde(rename = "type", default)]
          data_type: i32,
          #[serde(default)]
          s: Option<String>,
          #[serde(default)]
          b: Option<bool>,
          #[serde(default)]
          n: Option<f64>,
          #[serde(default)]
          a: Option<Vec<PipelineContextData>>,
          #[serde(default)]
          d: Option<Vec<DictEntry<PipelineContextData>>>,
        }
        let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
        Ok(PipelineContextData {
          data_type: inner.data_type,
          s: inner.s,
          b: inner.b,
          n: inner.n,
          a: inner.a,
          d: inner.d,
        })
      }
    }

    deserializer.deserialize_any(Visitor)
  }
}
