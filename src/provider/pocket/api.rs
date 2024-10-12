use crate::util::serialize::from_str;
use crate::util::serialize::option_bool_from_int_string;
use crate::util::serialize::option_string_date_unix_timestamp_format;
use crate::util::serialize::optional_vec_from_map;
use crate::util::serialize::serialize_as_string;
use crate::util::serialize::to_comma_delimited_string;
use crate::util::serialize::try_url_from_string;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Serialize)]
struct PocketOAuthRequest<'a> {
    consumer_key: &'a str,
    redirect_uri: &'a str,
    state: Option<&'a str>,
}

#[derive(Deserialize, Debug)]
struct PocketOAuthResponse {
    code: String,
    #[allow(dead_code)]
    state: Option<String>,
}

#[derive(Serialize)]
struct PocketAuthorizeRequest<'a> {
    consumer_key: &'a str,
    code: &'a str,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct PocketAuthorizeResponse {
    access_token: String,
    username: String,
    state: Option<String>,
}

/// Authenticate with Pocket
/// Returns the access token
pub async fn login(
    client: &reqwest::Client,
    consumer_key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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

    println!("Follow the url to provide access:\n{}", authorize_url);
    println!("Press enter to continue...");
    let _ = std::io::stdin().read_line(&mut String::new());

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

    let resp: PocketAuthorizeResponse = req.json().await?;
    println!("{:?}", resp);

    Ok(resp.access_token)
}

#[derive(Serialize)]
pub struct PocketRequest<'a, T> {
    consumer_key: &'a str,
    access_token: &'a str,
    #[serde(flatten)]
    request: T,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum PocketGetState {
    Unread,
    Archive,
    All,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum PocketGetSort {
    Newest,
    Oldest,
    Title,
    Site,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum PocketGetDetail {
    Simple,
    Complete,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct PocketGetParams {
    state: Option<PocketGetState>,
    sort: Option<PocketGetSort>,
    detail_type: Option<PocketGetDetail>,
    count: Option<u32>,
    offset: Option<u32>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum PocketResponse {
    Success(PocketGetResponse),
    Error { error: String },
}

#[derive(Deserialize, Debug)]
struct PocketGetResponse {
    list: Option<serde_json::Map<String, Value>>,
    // status: Option<u16>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ItemTag {
    #[serde(deserialize_with = "from_str")]
    pub item_id: u64,
    pub tag: String,
}

#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct PocketItem {
    #[serde(deserialize_with = "from_str")]
    pub item_id: u64,
    #[serde(default, deserialize_with = "try_url_from_string")]
    pub given_url: Option<Url>,
    #[serde(default)]
    pub given_title: Option<String>,

    #[serde(default, deserialize_with = "option_string_date_unix_timestamp_format")]
    pub time_added: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "option_string_date_unix_timestamp_format")]
    pub time_read: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "option_string_date_unix_timestamp_format")]
    pub time_updated: Option<DateTime<Utc>>,
    pub resolved_title: Option<String>,

    #[serde(default, deserialize_with = "option_bool_from_int_string")]
    pub favorite: Option<bool>,

    #[serde(default, deserialize_with = "try_url_from_string")]
    pub resolved_url: Option<Url>,

    #[serde(default, deserialize_with = "optional_vec_from_map")]
    pub tags: Option<Vec<ItemTag>>,
    pub excerpt: Option<String>,

    pub lang: Option<String>,
}

/// Isn't very reliable and subject to change without notice
/// @refer https://getpocket.com/developer/docs/v3/retrieve
pub async fn get(
    access_token: &str,
    consumer_key: &str,
    client: &reqwest::Client,
    limit: Option<usize>,
) -> Result<Vec<PocketItem>, Box<dyn std::error::Error>> {
    println!("Starting to fetch Pocket items");
    let mut all_items = Vec::new();
    let mut offset = 0;
    let count = 30; // Maximum items per request
    let mut empty_responses = 0;
    let max_empty_responses = 2; // Maximum number of consecutive empty responses before stopping

    loop {
        println!("Fetching items with offset: {}", offset);
        let body = &PocketRequest {
            access_token,
            consumer_key,
            request: PocketGetParams {
                state: Some(PocketGetState::All),
                sort: Some(PocketGetSort::Newest),
                detail_type: Some(PocketGetDetail::Complete),
                count: Some(count),
                offset: Some(offset),
            },
        };

        let req = client
            .post("https://getpocket.com/v3/get")
            .json(&body)
            .header("X-Accept", "application/json")
            .send()
            .await?;

        let status = req.status();
        println!("Received response with status: {}", status);

        let raw_response = req.text().await?;

        let resp: PocketResponse = serde_json::from_str(&raw_response)
            .map_err(|e| format!("Error parsing {} response: {}", raw_response, e))?;

        match resp {
            PocketResponse::Success(resp_json) => {
                if let Some(list) = resp_json.list {
                    let items: Vec<PocketItem> = list
                        .into_iter()
                        .filter_map(|(key, value)| {
                            match serde_json::from_value::<PocketItem>(value.clone()) {
                                Ok(item) => Some(item),
                                Err(e) => {
                                    eprintln!("Failed to parse item {}: {}", key, e);
                                    // Print the problematic fields
                                    if let Some(obj) = value.as_object() {
                                        for (field, field_value) in obj {
                                            if let Err(field_err) =
                                                serde_json::from_value::<serde_json::Value>(
                                                    field_value.clone(),
                                                )
                                            {
                                                eprintln!(
                                                    "  Field '{}' error: {}",
                                                    field, field_err
                                                );
                                            }
                                        }
                                    }
                                    eprintln!("Raw JSON: {}", value);
                                    None
                                }
                            }
                        })
                        .collect();

                    let items_count = items.len();
                    println!("Parsed {} items from response", items_count);

                    if items.is_empty() {
                        empty_responses += 1;
                        println!(
                            "Received empty list. Empty response count: {}",
                            empty_responses
                        );
                        if empty_responses >= max_empty_responses {
                            println!("Reached maximum number of consecutive empty responses. Breaking loop.");
                            break;
                        }
                    } else {
                        empty_responses = 0; // Reset the counter when we receive items
                        all_items.extend(items);
                    }

                    offset += count;
                    println!("Total items fetched so far: {}", all_items.len());

                    // Check if we've reached the limit
                    if let Some(limit) = limit {
                        if all_items.len() >= limit {
                            println!("Reached item limit. Breaking loop.");
                            all_items.truncate(limit);
                            break;
                        }
                    }
                } else {
                    println!(
                        "Received response with no list field. Continuing to next request."
                    );
                }
            }
            PocketResponse::Error { error } => {
                eprintln!("API returned error: {}", error);
                // Instead of returning immediately, we'll log the error and continue
                println!("Continuing to next request despite error.");
            }
        }

        println!("Sleeping before next request");
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    println!(
        "Finished fetching Pocket items. Total items: {}",
        all_items.len()
    );
    Ok(all_items)
}

#[derive(Serialize, Debug, Clone)]
pub struct PocketAddRequest<'a> {
    pub url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(serialize_with = "to_comma_delimited_string")]
    pub tags: Option<&'a [&'a str]>,
}

pub async fn add(
    client: &reqwest::Client,
    access_token: &str,
    consumer_key: &str,
    add_request: PocketAddRequest<'_>,
) -> Result<i64, Box<dyn std::error::Error>> {
    println!("Starting Pocket add request");

    let body = &PocketRequest {
        access_token,
        consumer_key,
        request: add_request,
    };

    let response = client
        .post("https://getpocket.com/v3/add")
        .json(&body)
        .header("X-Accept", "application/json")
        .send()
        .await?;

    if response.status().is_success() {
        println!("Successfully added item to Pocket");
        // get the item.item_id from the response
        let item_id = response.json::<serde_json::Value>().await?;
        let item_id = item_id
            .get("item")
            .ok_or("item object not found in response")?;
        let item_id = item_id
            .get("item_id")
            .ok_or("item_id not found in response")?;
        let item_id = item_id.as_str().ok_or("item_id not a string")?;
        Ok(item_id.parse()?)
    } else {
        let error_message = format!(
            "Failed to add item to Pocket. Status: {}",
            response.status()
        );
        println!("{}", error_message);
        Err(error_message.into())
    }
}

#[derive(Serialize)]
struct PocketSendRequest {
    actions: Vec<PocketFavoriteRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum SendAction {
    Favorite,
    Unfavorite,
}

#[derive(Serialize)]
struct PocketFavoriteRequest {
    #[serde(serialize_with = "serialize_as_string")]
    item_id: i64,
    action: SendAction,
    time: Option<String>,
}

pub async fn favorite(
    client: &reqwest::Client,
    access_token: &str,
    consumer_key: &str,
    item_id: i64,
    mark: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Pocket favorite request");

    let body = &PocketRequest {
        access_token,
        consumer_key,
        request: PocketSendRequest {
            actions: vec![PocketFavoriteRequest {
                item_id,
                time: None,
                action: if mark {
                    SendAction::Favorite
                } else {
                    SendAction::Unfavorite
                },
            }],
        },
    };

    let response = client
        .post("https://getpocket.com/v3/send")
        .json(&body)
        .header("X-Accept", "application/json")
        .send()
        .await?;

    if response.status().is_success() {
        println!("Successfully marked item as favorite in Pocket");
        Ok(())
    } else {
        let error_message = format!(
            "Failed to mark item as favorite in Pocket. Status: {}",
            response.status()
        );
        println!("{}", error_message);
        Err(error_message.into())
    }
}
