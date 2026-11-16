# Configuration imports and portable instance packs

In Server mode, select a saved draft and open **Configuration import and instance packs**. Choose its server, basic or dedicated profile file, then **Review configuration import**. The review shows a redacted diff, unknown fields, credential references and unresolved paths. Apply the reviewed import or cancel without changing the saved draft. Full replacement of an operator file uses the same review flow after editing the external source file.

Each file has an explicit ownership choice:

- **Operator** is the default. Foxy retains the exact UTF-8 text, comments, unknown directives/classes, ordering and line endings, replacing only secret value spans with protected references. The operator template controls rendering. Structured controls remain read-only; launch-only port and auto-init are still editable.
- **Managed** explicitly converts supported fields to Foxy's typed model and deterministic renderer. The review reports unsupported fields that conversion omits. It does not silently rewrite an operator file. Original input files remain untouched in both modes.

The Rust parser handles quoted strings with doubled quotes, literal backslashes, finite numeric values, arrays/tuples, nested classes, comments and byte spans. Includes, macros, opaque expressions and binary configurations are not evaluated. If they prevent safe inspection, import refuses to snapshot the file and leaves the original at its operator-owned source. Resolve the unsupported input before importing it. A syntax or mapping failure does not authorize a lossy replacement.

## CLI workflow

```text
Foxy server configuration import <id> --expected-revision 1 --server <server.cfg> --basic <basic.cfg> --profile <dedicated-profile> --json
Foxy server configuration import <id> --expected-revision 1 --server <server.cfg> --apply --yes --json
Foxy server configuration import <id> --expected-revision 2 --basic <basic.cfg> --managed --apply --yes --json
```

Use only the file roles you need. The default ownership is operator; `--managed` explicitly converts the supplied files. `--apply` requires `--yes`; omitting it only reviews. `--dry-run` never creates a draft, credential record or export file. All commands accept the existing `--game-space` selection. A changed saved revision or changed input file invalidates a retained desktop review.

Imported absolute/parent-relative paths are displayed as unresolved references. Foxy does not open, copy or resolve them. Remap them in the input file and review again, or acknowledge the exact paths with the desktop checkbox or CLI `--review-paths`. Acknowledgment is not a claim that a path exists. Activation remains blocked until path review and the normal installation/content/runtime gates pass.

## Protected credentials

Known password directives, including nested/case-varied names, are extracted before a preview or ordinary snapshot is created. Ordinary templates contain `foxy-secret:secret_<32 lowercase hex digits>` references. Credential values stay outside catalog/configuration archives and portable packs. Mask text cannot replace a stored password.

On Windows the credential store uses current-user DPAPI with instance/context binding. On Linux it uses same-user private directories and mode-0600 files; those files contain plaintext and depend on filesystem access control. Restricted runtime files are created only after Start admission and passed as configuration paths, keeping credential values out of process arguments. These files and the credential store are private runtime data, not portable exports. Unresolved references block activation. Protected directives in profile files remain blocked from native launch; put dedicated-server passwords in the server configuration.

Copying or importing an instance never copies credential values. To bind credentials for a new instance, import its actual local operator credential source. Do not paste secrets into command arguments or use redacted previews as password input. This storage is separate from Steam authentication and RCon session work.

## Portable packs

```text
Foxy server configuration export-pack <id> --file <instance.foxyserver> --json
Foxy server configuration export-pack <id> --file <sanitized.foxyserver> --sanitized --json
Foxy server configuration import-pack --file <instance.foxyserver> --name ImportedServer --json
Foxy server configuration import-pack --file <instance.foxyserver> --name ImportedServer --apply --yes --json
```

The desktop has matching **Export instance pack**, **Review pack import** and **Import as new draft** controls. Export creates a new file and refuses to overwrite an existing one. A `.foxyserver` file is bounded, versioned JSON, not an executable archive. The [reference pack](../examples/json/server_backend/arma3_instance_pack.json) contains only synthetic data and unresolved credential/path references.

Default exports preserve configuration definitions, safe operator templates, source references, addon roles/order and policy identifiers such as admin IDs. They exclude installation authority/binaries, live ownership, credential values, logs and saves. Existing content references are retained as unresolved intent; register local profile mappings and review content bindings/staging separately before deployment. The original source URL and local-path pair remain distinct identities.

Sanitized export removes operator templates, source/path hints, names, mission identities and text/list policy identifiers. Its `omitted_fields` lists every removed/replaced group. The result is explicitly incomplete and may require rebuilding access policy; it is not a ready-to-run server. Basic numeric tuning and custom difficulty remain available for comparison. Existing `.foxypack` client configuration packs keep their format and are not reinterpreted as server instance packs.

Import always creates a new instance identity at revision 1 with no installation binding. Configured ports are retained and overlaps are reported for review. Adopt/bind a local installation, choose unused ports, resolve credentials and review local content before preparing/activating it. Unknown directives remain in operator-owned templates across import/export. Selecting a different ownership mode or making a same-length edit still changes the reviewed content and fingerprint.
