# :crab: RUST PROJECT TEMPLATE - TODO(template) PUT PROJECT NAME HERE

## Overview

This repository serves as a template for starting new Rust projects at IgraLabs. It provides a standardized foundation with pre-configured settings, workflows, and recommended practices to ensure consistency and quality across our Rust codebase.

**Key Features:**

*   **Standardized Project Structure:** Basic layout including `src`, `tests`, `examples`, and configuration files.
*   **GitHub Actions Workflows:**
    *   `build-and-test.yml`: Builds the project and runs tests on every push, pull request, and merge queue.
    *   `linter.yml`: Runs `clippy`, `rustfmt`, and `typos` to enforce code style and catch common issues.
    *   `dependency-audit.yml`: Uses `cargo-deny` to check for dependencies with known security vulnerabilities.
    *   `ub-detection.yml`: Runs Miri to detect undefined behavior in unsafe code blocks.
    *   `docker-*.yml.template`: Optional Docker build and publish workflows (rename to enable).
*   **Repository Settings Recommendations:** The setup instructions guide you to configure repository settings for:
    *   Allowing only rebase merging for pull requests.
    *   Automatically deleting head branches after merging.
*   **Branch Protection:** Includes a default [ruleset](./.github/main-ruleset.json) (to be imported) for the main branch, enforcing:
    *   Pull requests are required before merging.
    *   At least one approving review is required.
    *   Code review conversation resolution is required.
    *   A linear commit history (no merge commits directly to the main branch).
    *   Deletion of the main branch is prevented.
    *   These rules apply even to administrators.
*   **Licensing:**
    *   The project itself defaults to the Apache 2.0 license (found in `LICENSE-APACHE`), but this should be reviewed and potentially updated (`TODO(template)`) for the specific project.
    *   Dependency licenses are enforced using `cargo-deny` via the [`deny.toml`](./deny.toml) configuration. Allowed licenses for dependencies are currently: MIT, Apache-2.0, BSD-3-Clause, BSD-2-Clause, CC-BY-1.0, CC-BY-2.0, CC-BY-3.0, CC-BY-4.0, ISC, OpenSSL, Unicode-3.0, Unicode-DFS-2016, Zlib.
*   **Contribution Guidelines:** Includes a basic `CONTRIBUTING.md`.


## TODO(template) - rust template usage (remove this section after setup)

This is a rust template from IgraLabs team :rocket:
To use it - find `TODO(template)` over the repository and set appropriate values.

- [ ] Settings -> Collaborators and teams - add your team group as admins for the repo
- [ ] Settings -> General -> Pull Requests - allow only `Allow rebase merging`, also tick `Automatically delete head branches`
- [ ] import protection [main ruleset json](./.github/main-ruleset.json) in the repo settings (Settings -> Rules -> Rulesets -> Import a ruleset)

## License

TODO(template) - update license

Apache 2.0

## Would like to contribute?

see [Contributing](./CONTRIBUTING.md).
