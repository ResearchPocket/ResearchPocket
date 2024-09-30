# Contributing to ResearchPocket

First off, thank you for considering contributing to ResearchPocket! It's people
like you that make ResearchPocket such a great tool.

If you'd like to skip to get started, check out the
[Getting Started](#getting-started) section.

## How Can I Contribute?

### Improving the Documentation

ResearchPocket could always use more documentation, whether as part of the
official ResearchPocket docs, in docstrings, or even on the web in blog posts,
articles, and such.

### Reporting Bugs

This section guides you through submitting a bug report for ResearchPocket.
Following these guidelines helps maintainers and the community understand your
report, reproduce the behavior, and find related reports.

- Use a clear and descriptive title for the issue to identify the problem.
- Describe the exact steps which reproduce the problem in as many details as
  possible.
- Provide specific examples to demonstrate the steps.

### Suggesting Enhancements

This section guides you through submitting an enhancement suggestion for
ResearchPocket, including completely new features and minor improvements to
existing functionality.

- Use a clear and descriptive title for the issue to identify the suggestion.
- Provide a step-by-step description of the suggested enhancement in as many
  details as possible.
- Explain why this enhancement would be useful to most ResearchPocket users.

### Your First Code Contribution

Unsure where to begin contributing to ResearchPocket? You can start by looking
through these `good-first-issue` and `help-wanted` issues:

- [Good First Issues](https://github.com/ResearchPocket/ResearchPocket/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
- [Help Wanted Issues](https://github.com/ResearchPocket/ResearchPocket/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)

### Pull Requests

- Fill in the required template
- Do not include issue numbers in the PR title
- Include screenshots and animated GIFs in your pull request whenever possible.
- Follow the Rust styleguides.
- Document new code based on the Documentation Styleguide
- End all files with a newline

## Styleguides

### Git Commit Messages

- Use the present tense ("Add feature" not "Added feature")
- Use the imperative mood ("Move cursor to..." not "Moves cursor to...")
- Limit the first line to 72 characters or less
- Reference issues and pull requests liberally after the first line

### Rust Styleguide

All Rust code must adhere to
[Rust Style Guide](https://github.com/rust-lang/rust/tree/master/src/doc/style-guide/src/SUMMARY.md).

## Additional Notes

### Issue and Pull Request Labels

This section lists the labels we use to help us track and manage issues and pull
requests.

- `bug` - Issues that are bugs.
- `enhancement` - Issues that are feature requests.
- `good first issue` - Good for newcomers.
- `help wanted` - Extra attention is needed.
- `question` - Further information is requested.

## Hacktoberfest

We welcome contributions as part of Hacktoberfest! Here are a few things to keep
in mind:

1. Quality over quantity: We value meaningful contributions that improve the
   project.
2. Follow the contribution guidelines: Make sure your pull requests adhere to
   our guidelines.
3. Be patient: We'll do our best to review your contributions in a timely
   manner, but it may take some time during the busy Hacktoberfest period.
4. Have fun and learn: Hacktoberfest is a great opportunity to learn and
   contribute to open source projects!

## Getting Started

To get started with developing ResearchPocket, follow these steps:

### Prerequisites

- Rust (latest stable version)
- Cargo (comes with Rust)
- SQLite

### Setting Up the Development Environment

1. Fork the repository on GitHub.
2. Clone your forked repository:

   ```sh
   git clone https://github.com/your-username/ResearchPocket.git
   cd ResearchPocket
   ```

3. Add the original repository as an upstream remote:

   ```sh
   git remote add upstream https://github.com/ResearchPocket/ResearchPocket.git
   ```

4. Install dependencies:
   ```sh
   cargo build
   ```

### Running the Application

To run the application in development mode:

```sh
cargo run
```

You can pass arguments to the application like this:

```sh
cargo run -- --help
cargo run -- init .
```

Thank you for contributing to ResearchPocket! Your efforts help make this
project better for everyone.
