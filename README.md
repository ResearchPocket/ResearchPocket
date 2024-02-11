<h1 align="center">Research Pocket 🔖</h1>
<div align="center">
  <strong>
    The <em>last</em> save-it-later tool you'll ever need
  </strong>
</div>
<br />
<div align="center">
  <!-- Github Actions -->
  <a href="https://github.com/launchbadge/sqlx/actions/workflows/sqlx.yml?query=branch%3Amain">
    <img src="https://img.shields.io/github/actions/workflow/status/KorigamiK/ResearchPocket/publish-release.yml?branch=main&style=flat-square" alt="actions status" /></a>
  <!-- Version -->
  <a href="https://crates.io/crates/sqlx">
    <img src="https://img.shields.io/crates/v/research.svg?style=flat-square"
    alt="Crates.io version" /></a>
  <!-- Docs -->
  <a href="https://docs.rs/research">
  <img src="https://img.shields.io/badge/docs-latest-blue.svg?style=flat-square" alt="docs.rs docs" /></a>
  <!-- Downloads -->
  <a href="https://crates.io/crates/sqlx">
    <img src="https://img.shields.io/crates/d/research.svg?style=flat-square" alt="Download" />
  </a>
</div>

<br/>

A self-hostable save-it-later tool that integrates with
[getpocket.com](https://getpocket.com) (and others soon). works on the web and
terminal

## How it works

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="./.github/explainer-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="./.github/explainer.png">
  <img alt="Hashnode logo" src="./.github/explainer.png" >
</picture>

## Cli

```sh
RESEARCH 🔖

Manage your reading lists and generate a static site with your saved articles.

Usage: research [OPTIONS] [COMMAND]

Commands:
  pocket    Pocket related actions
  fetch     Gets all data from authenticated providers
  list      Lists all items in the database
  init      Initializes the database
  generate  Generate a static site
  help      Print this message or the help of the given subcommand(s)

Options:
      --db <DB_URL>  Database url [env: DATABASE_URL=] [default: ./research.sqlite]
  -d, --debug...     Turn debugging information on
  -h, --help         Print help
  -V, --version      Print version
```

### Init

```sh
Initializes the database

Usage: research init <PATH>

Arguments:
  <PATH>  

Options:
  -h, --help  Print help
```

### Pocket

```sh
Pocket related actions

Usage: research pocket [COMMAND]

Commands:
  auth   Authenticate using a consumer key
  fetch  Fetch items from pocket
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### Generate

```sh
Generate a static site

Usage: research generate --assets <ASSETS_DIR> <PATH>

Arguments:
  <PATH>  

Options:
      --assets <ASSETS_DIR>  Path to site assets (main.css, search.js)
  -h, --help                 Print help
```

## Generate your site

This requires that you have
[tailwindcss](https://tailwindcss.com/blog/standalone-cli) installed and
available in your `$PATH`

```sh
$ research init # initializes the database
$ research pocket fetch # fetch your pocket data
$ research --db ./research.sqlite generate . --assets ./assets  # generate your site
```
