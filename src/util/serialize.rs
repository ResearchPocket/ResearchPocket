use chrono::{DateTime, TimeZone, Utc};
use serde::{de, Deserialize, Deserializer, Serializer};
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

pub fn from_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    D: serde::Deserializer<'de>,
{
    struct StringOrNumber<T>(std::marker::PhantomData<T>);

    impl<'de, T> serde::de::Visitor<'de> for StringOrNumber<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        type Value = T;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string or number")
        }

        fn visit_str<E>(self, value: &str) -> Result<T, E>
        where
            E: serde::de::Error,
        {
            T::from_str(value).map_err(serde::de::Error::custom)
        }

        fn visit_i64<E>(self, value: i64) -> Result<T, E>
        where
            E: serde::de::Error,
        {
            T::from_str(&value.to_string()).map_err(serde::de::Error::custom)
        }

        fn visit_u64<E>(self, value: u64) -> Result<T, E>
        where
            E: serde::de::Error,
        {
            T::from_str(&value.to_string()).map_err(serde::de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrNumber(std::marker::PhantomData))
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

pub fn option_bool_from_int_string<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(s) => match s.as_str() {
            "0" => Ok(Some(false)),
            "1" => Ok(Some(true)),
            _ => Err(de::Error::custom(format!("Invalid boolean value: {}", s))),
        },
        None => Ok(None),
    }
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

pub fn to_comma_delimited_string<S>(
    x: &Option<&[&str]>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match x {
        Some(value) => serializer.serialize_str(&value.join(",")),
        None => serializer.serialize_none(),
    }
}
