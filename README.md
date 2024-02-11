<h1 align="center">Research Pocket 🔖</h1>
<div align="center">
  <strong>
    The <em>last</em> save-it-later tool you'll ever need
  </strong>
</div>

<br />

A self-hostable save-it-later tool that integrates with
[getpocket.com](https://getpocket.com) (and others soon). works on the web and terminal

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

## Generating your site

```sh
$ research init # initializes the database
$ research pocket fetch # fetch your pocket data
$ research --db ./research.sqlite generate . --assets ./assets  # generate your site
```
