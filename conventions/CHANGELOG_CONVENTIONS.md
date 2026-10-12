\### Changelog rules

\- Only edit `CHANGELOG.md` when explicitly asked.

\- When asked to update the changelog, target the version specified by the user.

\- If no version is specified, update the highest (latest) version already present in `CHANGELOG.md`.

\- Keep the latest release first, with a `# MAJOR.MINOR.PATCH` heading. Foxy's build script derives the application version from this heading and rejects missing or invalid versions. Cargo package metadata is separate from the application release version.

\- Keep sections in this exact order:

&#x20; - `## Added`

&#x20; - `## Changed`

&#x20; - `## Fixed`

&#x20; - `## Removed`

&#x20; - `## Reverted`

\- Write changelog entries for end-user readability first: brief, clear, and outcome-focused.

\- Do not mention fixes/changes that were for the features for that given version (For example Added feature X and Fixed feature X) which would lead to confusing changelog because that feature was actually not in the previous version.

