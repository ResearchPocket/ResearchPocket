use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serializer};
use serde_json::Value;
use url::Url;

pub fn option_string_date_unix_timestamp_format<'de, D>(
    deserializer: D,
) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::deserialize(deserializer).and_then(|o: Option<String>| match o.as_deref() {
        Some("0") | None => Ok(None),
        Some(str) => str
            .parse::<i64>()
            .map(|i| Some(Utc.timestamp_opt(i, 0).unwrap()))
            .map_err(serde::de::Error::custom),
    })
}

pub fn string_date_unix_timestamp_format<'de, D>(
    deserializer: D,
) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse::<i64>()
        .map(|i| Utc.timestamp_opt(i, 0).unwrap())
        .map_err(serde::de::Error::custom)
}

pub fn from_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    T::from_str(&s).map_err(serde::de::Error::custom)
}

pub fn try_url_from_string<'de, D>(deserializer: D) -> Result<Option<Url>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let o: Option<String> = Option::deserialize(deserializer)?;
    Ok(o.and_then(|s| Url::parse(&s).ok()))
}

fn json_value_to_vec<'de, T, D>(value: Value) -> Result<Vec<T>, D::Error>
where
    T: serde::de::DeserializeOwned + Clone + std::fmt::Debug,
    D: serde::Deserializer<'de>,
{
    match value {
        a @ Value::Array(..) => {
            serde_json::from_value::<Vec<T>>(a).map_err(serde::de::Error::custom)
        }
        o @ Value::Object(..) => {
            serde_json::from_value::<std::collections::BTreeMap<String, T>>(o)
                .map(map_to_vec)
                .map_err(serde::de::Error::custom)
        }
        other => Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Other(format!("{:?}", other).as_str()),
            &"object or array",
        )),
    }
}

fn map_to_vec<T>(map: std::collections::BTreeMap<String, T>) -> Vec<T> {
    map.into_values().collect::<Vec<_>>()
}
pub fn bool_from_int_string<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match String::deserialize(deserializer)?.as_str() {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Str(other),
            &"zero or one",
        )),
    }
}
pub fn vec_from_map<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    T: serde::de::DeserializeOwned + Clone + std::fmt::Debug,
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    json_value_to_vec::<T, D>(value)
}

pub fn optional_vec_from_map<'de, T, D>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    T: serde::de::DeserializeOwned + Clone + std::fmt::Debug,
    D: serde::Deserializer<'de>,
{
    let o: Option<Value> = Option::deserialize(deserializer)?;
    match o {
        Some(v) => json_value_to_vec::<T, D>(v).map(Some),
        None => Ok(None),
    }
}

pub fn to_comma_delimited_string<S>(x: &Option<&[&str]>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match x {
        Some(value) => serializer.serialize_str(&value.join(",")),
        None => serializer.serialize_none(),
    }
}
