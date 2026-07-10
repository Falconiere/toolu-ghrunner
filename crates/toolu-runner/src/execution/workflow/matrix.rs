use std::collections::HashMap;

use super::types::MatrixConfig;

/// Expand a matrix configuration into all combinations.
///
/// Computes Cartesian product of base keys, applies exclude, then include.
pub fn expand_matrix(config: &MatrixConfig) -> Vec<HashMap<String, String>> {
  let mut combos = cartesian_product(&config.base);

  // Apply exclude: remove combinations where ALL specified keys match
  combos.retain(|combo| !config.exclude.iter().any(|exc| matches_exclude(combo, exc)));

  // Apply include: merge into matching combos or add as new
  for inc in &config.include {
    let inc_strings: HashMap<String, String> = inc
      .iter()
      .map(|(k, v)| (k.clone(), yaml_value_to_string(v)))
      .collect();

    let matched = combos.iter_mut().any(|combo| {
      let all_match = inc_strings
        .iter()
        .all(|(k, v)| combo.get(k).is_none_or(|cv| cv == v));
      if all_match && !inc_strings.is_empty() {
        combo.extend(inc_strings.clone());
        true
      } else {
        false
      }
    });

    if !matched {
      combos.push(inc_strings);
    }
  }

  if combos.is_empty() {
    combos.push(HashMap::new());
  }

  combos
}

fn cartesian_product(
  base: &HashMap<String, Vec<serde_yaml::Value>>,
) -> Vec<HashMap<String, String>> {
  let keys: Vec<&String> = base.keys().collect();
  if keys.is_empty() {
    return vec![HashMap::new()];
  }

  let mut result = vec![HashMap::new()];

  for key in &keys {
    let values = base.get(*key).map(Vec::as_slice).unwrap_or_default();
    let mut new_result = Vec::new();

    for combo in &result {
      for val in values {
        let mut new_combo = combo.clone();
        new_combo.insert((*key).clone(), yaml_value_to_string(val));
        new_result.push(new_combo);
      }
    }

    result = new_result;
  }

  result
}

fn matches_exclude(
  combo: &HashMap<String, String>,
  exclude: &HashMap<String, serde_yaml::Value>,
) -> bool {
  exclude.iter().all(|(k, v)| {
    combo
      .get(k)
      .is_some_and(|cv| cv == &yaml_value_to_string(v))
  })
}

fn yaml_value_to_string(value: &serde_yaml::Value) -> String {
  match value {
    serde_yaml::Value::String(s) => s.clone(),
    serde_yaml::Value::Number(n) => n.to_string(),
    serde_yaml::Value::Bool(b) => b.to_string(),
    serde_yaml::Value::Null => String::new(),
    serde_yaml::Value::Sequence(_)
    | serde_yaml::Value::Mapping(_)
    | serde_yaml::Value::Tagged(_) => format!("{value:?}"),
  }
}
