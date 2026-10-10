//! Custom `Deserialize` impl for [`TemplateToken`].
//!
//! GitHub serializes a template token as a bare string/bool/number/null when
//! it carries no source position, and as a typed object (`type` plus `lit` /
//! `bool` / `num` / `expr` / `seq` / `map`, usually with `file`/`line`/`col`)
//! otherwise. A typed object without `type` is a string token. Source
//! positions are retained for diagnostics.
//!
//! The shape is only known from the value itself, so this needs a
//! self-describing format (`deserialize_any`). The job message is always
//! JSON; non-self-describing formats such as bincode are unsupported.

use serde::Deserialize;
use serde::de;

use super::context_data::DictEntry;
use super::template_token::TemplateToken;

impl<'de> Deserialize<'de> for TemplateToken {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
      type Value = TemplateToken;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a TemplateToken object, string, bool, number, or null")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<TemplateToken, E> {
        Ok(TemplateToken::literal(v))
      }

      fn visit_bool<E: de::Error>(self, v: bool) -> Result<TemplateToken, E> {
        Ok(TemplateToken::boolean(v))
      }

      fn visit_i64<E: de::Error>(self, v: i64) -> Result<TemplateToken, E> {
        Ok(TemplateToken::number(v as f64))
      }

      fn visit_u64<E: de::Error>(self, v: u64) -> Result<TemplateToken, E> {
        Ok(TemplateToken::number(v as f64))
      }

      fn visit_f64<E: de::Error>(self, v: f64) -> Result<TemplateToken, E> {
        Ok(TemplateToken::number(v))
      }

      fn visit_unit<E: de::Error>(self) -> Result<TemplateToken, E> {
        Ok(TemplateToken::null())
      }

      fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<TemplateToken, A::Error> {
        #[derive(Deserialize)]
        struct Inner {
          #[serde(rename = "type", default)]
          token_type: i32,
          #[serde(default)]
          lit: Option<String>,
          #[serde(default)]
          expr: Option<String>,
          #[serde(default, rename = "bool")]
          bool_val: Option<bool>,
          #[serde(default, rename = "num")]
          num_val: Option<f64>,
          #[serde(default, alias = "map")]
          d: Option<Vec<DictEntry<TemplateToken>>>,
          #[serde(default)]
          seq: Option<Vec<TemplateToken>>,
          #[serde(default)]
          file: Option<i32>,
          #[serde(default)]
          line: Option<i32>,
          #[serde(default)]
          col: Option<i32>,
        }
        let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
        Ok(TemplateToken {
          token_type: inner.token_type,
          lit: inner.lit,
          expr: inner.expr,
          bool_val: inner.bool_val,
          num_val: inner.num_val,
          d: inner.d,
          seq: inner.seq,
          file: inner.file,
          line: inner.line,
          col: inner.col,
        })
      }
    }

    // Requires a self-describing format such as JSON; bincode cannot drive it.
    deserializer.deserialize_any(Visitor)
  }
}
