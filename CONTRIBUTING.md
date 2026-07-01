# Contributing to Foxy

Foxy is source-available under the Foxy Community Source License 1.0.0.

The project is designed to be contribution-friendly while actively maintained, and community-continuable if it becomes inactive.

## Official communication

All official project communication for licensing, maintainer status, community continuation, maintainer return, commercial permission, and governance happens through GitHub Issues in the official repository.

Do not rely on email, private messages, chat, social media, or comments outside GitHub Issues as official project communication unless an official GitHub issue explicitly designates that channel for that specific matter.

## What you may do

You may:

1. read and study the source;
2. run Foxy for noncommercial purposes;
3. make private modifications for noncommercial purposes;
4. create a public GitHub fork to prepare a contribution;
5. submit pull requests and issues;
6. continue the project under the Foxy Community Source License if the inactivity process in `LICENSE` is completed.

## What you may not do while Foxy is active

You may not publish an independent public distribution, installer, package feed, rebrand, hosted service, or separately maintained public product based on Foxy while the official project is actively maintained, unless an official maintainer permits it through GitHub Issues.

Commercial use requires separate written permission from the Project Steward.

## Contribution terms

By submitting a pull request, patch, issue text intended for inclusion, documentation, test, asset, design, or other contribution to Foxy, you agree to `CLA.md`.

Do not submit a contribution if you cannot grant the rights in `CLA.md`.

If your employer, client, school, or another organization may own your work, get permission before submitting it.

If your contribution includes third-party material, clearly identify the source and license.

### Accepting the CLA (required)

Acceptance of `CLA.md` is required before a pull request can be merged. You accept it by doing both of the following:

1. checking the Contributor License Agreement checkbox in the pull request template; and
2. signing off every commit under the Developer Certificate of Origin with `git commit -s`, which adds a `Signed-off-by` line.

If you forget the sign-off, you can add it to the latest commit with `git commit --amend -s` and force-push your branch, or sign off a range of commits with `git rebase --signoff`.

The sign-off is enforced automatically: the **DCO** status check runs on every pull request and verifies that each commit has a `Signed-off-by` line matching its author. A pull request cannot be merged until that check passes, so expect a failing check if any commit is unsigned, and use the commands above to fix it.

## Pull requests

Pull requests should be focused, reviewable, and connected to an issue when possible.

A contribution fork should exist for contribution work only. Do not publish releases, installers, package feeds, binaries, marketplace listings, or branded distributions from a contribution fork unless an official maintainer permits it through GitHub Issues.

## Community continuation

If there has been no Meaningful Update for 24 consecutive months, anyone may open a GitHub issue titled:

```text
[Community Continuation Request]
```

The maintainer then has 90 days to provide a Valid Maintainer Response as defined in `LICENSE`.

If no Valid Maintainer Response is completed in time, the community may continue Foxy under the same Foxy Community Source License 1.0.0. The project does not convert to Apache-2.0, MIT, or another license.

## Maintainer return

If a previous Project Steward returns after community continuation, they may open a GitHub issue titled:

```text
[Maintainer Return Request]
```

The current Community Continuation Maintainer must respond in good faith within 90 days as defined in `LICENSE`.

The return process may repeat across multiple maintainers. Anyone who takes over Foxy as a community continuation maintainer accepts that later handback, shared maintenance, or succession may happen through GitHub Issues.
