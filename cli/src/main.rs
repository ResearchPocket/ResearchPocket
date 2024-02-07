use clap::{arg, command, Command};
use providers::pocket;

mod providers;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .arg(arg!(-d --debug ... "Turn debugging information on"))
        .subcommand(
            Command::new("login")
                .about("does login things")
                .arg(arg!([token])),
        )
        .subcommand(Command::new("fetch").about("Gets all pocket data"))
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
        pocket::login(&client, &consumer_key).await?;
    }

    if let Some(_) = matches.subcommand_matches("fetch") {
        let consumer_key = std::env::var("POCKET_CONSUMER_KEY").expect("Required consumer key");
        let access_token = std::env::var("POCKET_ACCESS_TOKEN").expect("required access token");
        let client = reqwest::Client::new();
        pocket::get(&access_token, &consumer_key, &client).await?;
    }

    Ok(())
}
