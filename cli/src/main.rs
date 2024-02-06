use clap::{arg, command, Command};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

async fn login(
    client: &reqwest::Client,
    consumer_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    pub struct PocketOAuthRequest<'a> {
        consumer_key: &'a str,
        redirect_uri: &'a str,
        state: Option<&'a str>,
    }

    #[derive(Deserialize, Debug)]
    pub struct PocketOAuthResponse {
        code: String,
        #[allow(dead_code)]
        state: Option<String>,
    }

    let body = PocketOAuthRequest {
        consumer_key,
        redirect_uri: "0.0.0.0",
        state: Some("pocket-research"),
    };
    let req = client
        .post("https://getpocket.com/v3/oauth/request")
        .json(&body)
        .header("X-Accept", "application/json")
        .send()
        .await?;
    let resp = req.json::<PocketOAuthResponse>().await?;
    let code = resp.code;

    let authorize_url = {
        let params = vec![
            ("request_token", code.clone()),
            ("redirect_uri", "0.0.0.0".into()),
        ];
        let mut url = Url::parse("https://getpocket.com/auth/authorize").unwrap();
        url.query_pairs_mut().extend_pairs(params.into_iter());
        url
    };

    println!("Follow the url to provide access: {}", authorize_url);
    let _ = std::io::stdin().read_line(&mut String::new());

    #[derive(Serialize)]
    pub struct PocketAuthorizeRequest<'a> {
        consumer_key: &'a str,
        code: &'a str,
    }

    let body = &PocketAuthorizeRequest {
        consumer_key,
        code: &code,
    };

    let req = client
        .post("https://getpocket.com/v3/oauth/authorize")
        .json(&body)
        .header("X-Accept", "application/json")
        .send()
        .await?;

    #[derive(Deserialize, Debug, PartialEq)]
    pub struct PocketAuthorizeResponse {
        access_token: String,
        username: String,
        state: Option<String>,
    }
    let resp: PocketAuthorizeResponse = req.json().await?;
    println!("{:?}", resp);

    Ok(())
}

async fn get(
    access_token: &str,
    consumer_key: &str,
    client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct PocketRequest<'a> {
        consumer_key: &'a str,
        access_token: &'a str,
        #[serde(flatten)]
        request: PocketGetParams,
    }

    #[derive(Serialize, Debug)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    pub enum PocketGetState {
        Unread,
        Archive,
        All,
    }

    #[derive(Serialize, Debug)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    pub enum PocketGetSort {
        Newest,
        Oldest,
        Title,
        Site,
    }

    #[derive(Serialize, Debug)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    pub enum PocketGetDetail {
        Simple,
        Complete,
    }
    #[derive(Serialize, Default)]
    struct PocketGetParams {
        state: Option<PocketGetState>,
        sort: Option<PocketGetSort>,
        detail_type: Option<PocketGetDetail>,
    }

    let body = &PocketRequest {
        access_token,
        consumer_key,
        request: PocketGetParams {
            state: Some(PocketGetState::All),
            sort: Some(PocketGetSort::Newest),
            detail_type: Some(PocketGetDetail::Complete),
        },
    };

    fn vec_from_map<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
    where
        T: serde::de::DeserializeOwned + Clone + std::fmt::Debug,
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        json_value_to_vec::<T, D>(value)
    }

    #[derive(Deserialize, Debug, PartialEq)]
    struct PocketGetResponse {
        #[serde(deserialize_with = "vec_from_map")]
        list: Vec<PocketItem>,
        status: u16,
        // #[serde(deserialize_with = "bool_from_int")]
        // complete: bool,
        error: Option<String>,
        // search_meta: PocketSearchMeta,
        // #[serde(deserialize_with = "int_date_unix_timestamp_format")]
        // since: DateTime<Utc>,
    }

    #[derive(Deserialize, Debug, PartialEq, Clone, Copy)]
    pub enum PocketItemStatus {
        #[serde(rename = "0")]
        Normal,
        #[serde(rename = "1")]
        Archived,
        #[serde(rename = "2")]
        Deleted,
    }
    fn try_url_from_string<'de, D>(deserializer: D) -> Result<Option<Url>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let o: Option<String> = Option::deserialize(deserializer)?;
        Ok(o.and_then(|s| Url::parse(&s).ok()))
    }
    fn from_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        T::from_str(&s).map_err(serde::de::Error::custom)
    }

    fn map_to_vec<T>(map: std::collections::BTreeMap<String, T>) -> Vec<T> {
        map.into_iter().map(|(_, v)| v).collect::<Vec<_>>()
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

    #[derive(Deserialize, Debug, Clone, PartialEq)]
    pub struct ItemTag {
        #[serde(deserialize_with = "from_str")]
        pub item_id: u64,
        pub tag: String,
    }

    fn bool_from_int_string<'de, D>(deserializer: D) -> Result<bool, D::Error>
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

    fn optional_vec_from_map<'de, T, D>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
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
    #[derive(Deserialize, Debug, PartialEq, Clone)]
    pub struct PocketItem {
        #[serde(deserialize_with = "from_str")]
        pub item_id: u64,

        #[serde(default, deserialize_with = "try_url_from_string")]
        pub given_url: Option<Url>,
        pub given_title: String,

        // #[serde(with = "string_date_unix_timestamp_format")]
        pub time_added: String,
        // #[serde(deserialize_with = "option_string_date_unix_timestamp_format")]
        pub time_read: Option<String>,
        // #[serde(with = "string_date_unix_timestamp_format")]
        pub time_updated: String,
        // #[serde(deserialize_with = "option_string_date_unix_timestamp_format")]
        pub time_favorited: Option<String>,

        #[serde(deserialize_with = "bool_from_int_string")]
        pub favorite: bool,

        // #[serde(deserialize_with = "bool_from_int_string")]
        // pub is_index: bosol,
        // #[serde(deserialize_with = "bool_from_int_string")]
        // pub is_article: bool,
        #[serde(deserialize_with = "from_str")]
        pub resolved_id: u64,
        pub resolved_title: Option<String>,
        #[serde(default, deserialize_with = "try_url_from_string")]
        pub resolved_url: Option<Url>,

        pub sort_id: u64,

        pub status: PocketItemStatus,
        #[serde(default, deserialize_with = "optional_vec_from_map")]
        pub tags: Option<Vec<ItemTag>>,
        // #[serde(default, deserialize_with = "optional_vec_from_map")]
        // pub images: Option<Vec<PocketImage>>,
        // #[serde(default, deserialize_with = "optional_vec_from_map")]
        // pub videos: Option<Vec<ItemVideo>>,
        // #[serde(default, deserialize_with = "optional_vec_from_map")]
        // pub authors: Option<Vec<ItemAuthor>>,
        pub lang: Option<String>,
        pub time_to_read: Option<u64>,
        pub listen_duration_estimate: Option<u64>,
        #[serde(default, deserialize_with = "try_url_from_string")]
        pub amp_url: Option<Url>,
        #[serde(default, deserialize_with = "try_url_from_string")]
        pub top_image_url: Option<Url>,
    }

    let req = client
        .post("https://getpocket.com/v3/get")
        .json(&body)
        .header("X-Accept", "application/json")
        .send()
        .await?;

    let resp: PocketGetResponse = req.json().await?;
    println!("{:#?}", resp);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .arg(arg!( -d --debug ... "Turn debugging information on"))
        .subcommand(
            Command::new("login")
                .about("does login things")
                .arg(arg!([token])),
        )
        .subcommand(Command::new("get").about("Gets all pocket data"))
        .get_matches();

    if let Some(matches) = matches.subcommand_matches("login") {
        let consumer_key = matches
            .get_one::<String>("token")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                std::env::var("POCKET_CONSUMER_KEY").expect("A consumer_key is required")
            });

        println!("token: {consumer_key:?}");
        let client = reqwest::Client::new();
        login(&client, &consumer_key).await?;
    }

    if let Some(_) = matches.subcommand_matches("get") {
        let consumer_key = std::env::var("POCKET_CONSUMER_KEY").expect("Required consumer key");
        let access_token = std::env::var("POCKET_ACCESS_TOKEN").expect("required access token");
        let client = reqwest::Client::new();
        get(&access_token, &consumer_key, &client).await?;
    }

    Ok(())
}
