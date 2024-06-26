use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct CliArgs {
    /// Database url
    #[arg(long, env = "DATABASE_URL", default_value = "./research.sqlite")]
    pub db: String,

    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub debug: u8,

    #[command(subcommand)]
    pub subcommand: Option<Subcommands>,
}

#[derive(Subcommand)]
pub enum Subcommands {
    /// Pocket related actions
    Pocket {
        #[clap(subcommand)]
        command: PocketCommands,
    },

    Local {
        #[clap(subcommand)]
        command: LocalCommands,
    },

    /// Gets all data from authenticated providers
    Fetch,

    /// Lists all items in the database
    List {
        /// Filter by tags separated by commas
        /// Example: --tag rust,sql
        #[clap(short, long, value_delimiter = ',', num_args = 1.. )]
        tag: Option<Vec<String>>,
    },

    /// Initializes the database
    #[command(arg_required_else_help = true)]
    Init {
        /// This path will be used to create the database file and SAVED for future use.
        #[arg(index = 1, required = true)]
        path: String,
    },

    /// Generate a static site
    #[command(arg_required_else_help = true)]
    Generate {
        /// The path to the output directory
        #[arg(index = 1, required = true)]
        output: String,

        /// Path to required site assets (main.css, search.js, tailwind.config.js)
        #[arg(long, default_value = "./assets")]
        assets: String,

        /// Download Tailwind binary to <ASSETS>/tailwindcss if not found
        #[arg(long, action = clap::ArgAction::SetTrue)]
        download_tailwind: bool,
    },
}

#[derive(Args)]
pub struct AuthArgs {
    /// Consumer key (https://getpocket.com/developer/apps/new)
    #[arg(short, long, env = "POCKET_CONSUMER_KEY", required = true)]
    pub key: String,
}

#[derive(Args)]
pub struct FetchArgs {
    /// Pocket Consumer key
    #[arg(long, env = "POCKET_CONSUMER_KEY", required = true)]
    pub key: String,

    /// Pocket Access token
    #[arg(long, env = "POCKET_ACCESS_TOKEN", required = true)]
    pub access: String,
}

#[derive(Subcommand)]
pub enum PocketCommands {
    /// Authenticate using a consumer key
    Auth(AuthArgs),

    /// Fetch items from pocket
    Fetch(FetchArgs),
}

#[derive(Args)]
pub struct AddArgs {
    /// URI (link) of the item (required)
    pub uri: String,

    /// Title of the item
    pub title: Option<String>,

    /// Excerpt of the item
    pub excerpt: Option<String>,

    /// Tags to associate with the item (comma separated)
    #[clap(short, long, value_delimiter = ',', num_args = 1.. )]
    pub tag: Option<Vec<String>>,
}

#[derive(Subcommand)]
pub enum LocalCommands {
    /// Add an item to the local provider in the database
    Add(AddArgs),

    /// List all items in the local provider
    List,
}
