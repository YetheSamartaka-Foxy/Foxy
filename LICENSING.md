# Licensing and Redistribution - Foxy

Foxy is licensed under the Foxy Community Source License 1.0.0.

Copyright (c) 2026 Yethe Samartaka

Official project communication happens through GitHub Issues in the official repository.

## 1. Practical summary

Foxy is not all-rights-reserved source code. People may read it, run it for noncommercial purposes, privately modify it, and create contribution forks.

Foxy is also not an open-fork project while it is actively maintained. Independent public distributions, package feeds, installers, rebrands, hosted services, and separately maintained public branches are not allowed while the official project is active, unless an official maintainer permits them through GitHub Issues.

If the official project becomes inactive, the community can continue Foxy under the same Foxy Community Source License 1.0.0. Foxy does not convert to Apache-2.0, MIT, or another license.

## 2. Why this license exists

The goal is to avoid two bad outcomes:

1. an active project fragmenting into many public forks and unofficial distributions;
2. a visible-source project becoming legally frozen because the maintainer disappears and nobody has permission to continue it.

The Foxy Community Source License keeps the project centralized while it is active, but gives the community a defined continuation path if maintainers stop maintaining it.

## 3. Active-maintenance model

While Foxy is active, users may:

1. read the source;
2. run Foxy for noncommercial purposes;
3. make private noncommercial modifications;
4. create public GitHub forks for contribution work;
5. submit issues and pull requests.

While Foxy is active, users may not:

1. use Foxy commercially without separate permission;
2. publish independent public distributions;
3. publish installers, binaries, package feeds, update channels, marketplace listings, or hosted services;
4. maintain a separate public product or rebrand based on Foxy;
5. imply official status without permission.

## 4. Inactivity and community continuation

A Community Continuation Request can be opened only after 24 consecutive months without a Meaningful Update.

The request must be a public GitHub issue titled:

```text
[Community Continuation Request]
```

The maintainer then has 90 days to provide a Valid Maintainer Response.

A valid response must do one of the following within those 90 days:

1. publish a Meaningful Update;
2. identify a Meaningful Update completed during the previous 24 months;
3. appoint a successor Project Steward with practical authority;
4. state that Foxy is no longer actively maintained and community continuation is allowed;
5. open a public maintainer search and appoint a successor Project Steward within the same 90-day period.

A vague roadmap or promise is not enough.

If no Valid Maintainer Response is completed within 90 days, a Community Continuation Event occurs.

After that event, the community may continue Foxy under the Foxy Community Source License 1.0.0. The license does not change to Apache-2.0, MIT, or another license.

## 5. Maintainer return and succession

If a previous Project Steward returns after a Community Continuation Event, they may open a public GitHub issue titled:

```text
[Maintainer Return Request]
```

The current Community Continuation Maintainer must respond in good faith within 90 days.

The response may return stewardship, create shared maintenance, appoint the previous maintainer, agree on a staged handback, decline with a clear public explanation, or appoint another successor.

This can repeat across multiple maintainers. Every new maintainer takes over Foxy with the understanding that later handback, shared maintenance, succession, or another inactivity process may happen.

A returning maintainer cannot revoke rights that already became available under the Foxy Community Source License after a Community Continuation Event. A return changes project stewardship only by public agreement. It does not convert the project to another license.

## 6. Contributor rights

Contributors retain copyright in their contributions, but accepted contributions require rights broad enough for Foxy to be maintained, commercially licensed, continued by the community, and transferred through maintainer succession.

For that reason, contributions are accepted under `CLA.md`.

A departing contributor should not be able to shut down the project by revoking rights to already accepted contributions.

## 7. Commercial permission

Commercial use is not granted by the Foxy Community Source License 1.0.0.

Commercial permission requests must begin as public GitHub issues titled:

```text
[Commercial Permission Request]
```

The Project Steward may then decline, discuss publicly, or designate another channel for private terms in that issue.

## 8. Governing law

The Foxy Community Source License 1.0.0 and the Foxy Contributor License Agreement 1.0.0 are governed by the laws of the Czech Republic, consistent with Regulation (EC) No 593/2008 (Rome I), with the courts of the Czech Republic having jurisdiction subject to mandatory rules. This choice does not remove mandatory consumer or other non-derogable protections under European Union law or a person's country of habitual residence. See `LICENSE` Section 15 and `CLA.md` Section 10.

## 9. Third-party licenses

The Foxy Community Source License applies to first-party Foxy code and materials. It does not relicense dependencies or third-party assets.

When shipping binaries, installers, packages, or public continuation builds, maintainers must include all required third-party notices and license texts.

The repository already contains the cargo-about configuration (`about.toml`) and the plain-text template (`about.hbs`). Do not run `cargo about init`; it would overwrite them. To regenerate the notices before a release:

```bash
# The cargo-about binary requires the `cli` feature.
cargo install cargo-about --locked --features cli
cargo about generate about.hbs -o THIRD-PARTY-LICENSES.txt
```

If `cargo about generate` reports an unmatched or unexpected license, review that dependency and, if its license is acceptable, add the SPDX identifier to the `accepted` list in `about.toml`, then regenerate.

The release process should ship `THIRD-PARTY-LICENSES.txt` next to the binary or installer and link it from the About screen if the application has one.

## 10. Repository files

Recommended root files:

```text
LICENSE
CLA.md
CONTRIBUTING.md
LICENSING.md
THIRD-PARTY-LICENSES.txt
```

`THIRD-PARTY-LICENSES.txt` should be generated from the actual dependency lockfile before release.

## 11. Cargo metadata

For Rust package metadata, prefer:

```toml
license-file = "LICENSE"
```

If publishing to a registry, check that registry's current policy for custom licenses first.
