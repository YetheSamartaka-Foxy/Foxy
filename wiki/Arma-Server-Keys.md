# Arma server signing keys

Foxy's **Server signature keys** section reviews Arma 3 public `.bikey` files for one server instance. It can collect candidates from the adopted installation's `keys` folder, one explicitly selected content stage, and operator-selected files. It reports each candidate's filename, authority label from the BIKEY, RSA size, key fingerprint, file BLAKE3 hash, and source provenance. The authority label is not proof of publisher identity. The parser checks the public BIKEY structure and rejects private-key blobs. This is inventory and identity evidence; the review does not verify addon signatures or establish that the engine will admit a client.

Review is read-only. Select the keys to trust with the individual checkboxes, then apply the reviewed policy. The policy is a replacement set for that server instance: previously trusted keys appear as candidates, but are not automatically retained just because they were trusted before. The exact tested base `a3.bikey` is retained implicitly. Any other key in the effective installation base must remain selected. Key trust is per instance, and an imported or cloned instance must have its key candidates reviewed for that instance again.

## How the policy is used

The native runtime retains the engine's normal base key directory and adds Foxy's private `foxy-keys` folder through `-keysFolder`. Selected public key material is copied into that instance's immutable runtime generation. Foxy does not add `!keys`, which would exclude the default base keys. This follows the engine's `keysFolder` behavior: it adds key folders, while the base game `keys` folder is included by default unless explicitly excluded. See [Arma 3 startup parameters](https://community.bistudio.com/wiki/Arma_3%3A_Startup_Parameters).

Addon loading roles remain separate from key trust. Required content is placed in the runtime's `-mod` list; server-only content is placed in `-serverMod`. Optional sources are not loaded into the runtime and remain recommendations, whether or not a public key from them is trusted. Trusting a key does not change addon role, load a mod, or toggle client-side addon checkboxes. Mission library content and mission wrappers are not addon signing keys.

The trust policy is part of the instance draft and is captured in later configuration archives and native runtime generations. Applying a policy changes the draft only; it does not alter an already committed runtime. Since the draft revision changes, bindings and stages reviewed before the policy apply are no longer current. Review and save the content bindings, stage, and configuration archive again before composing and activating a new deployment. Generation rollback uses the retained generation state; it does not rewrite source key files or client addon selections.

The server's `verifySignatures` setting is separate from the key review. The engine configuration controls whether addon signature verification is enabled; Foxy's current setting values include disabled and v2. The review displays that setting but does not perform the cryptographic signature check. See [Bohemia's server configuration reference](https://community.bohemia.net/wiki/Arma_3%3A_Server_Config_File). Do not treat a key inventory, saved trust policy, or successful preparation as proof of signature acceptance. That requires supported native engine acceptance evidence.

## CLI

```text
Foxy server keys audit <instance_id> --json
Foxy server keys audit <instance_id> --stage-id <stage_id> --operator-key <file.bikey> --json
Foxy server keys audit <instance_id> --operator-key <file.bikey> --apply --yes --fingerprint <review-fingerprint> --trust <candidate-id> --json
Foxy --dry-run server keys audit <instance_id> --operator-key <file.bikey> --apply --yes --fingerprint <review-fingerprint> --trust <candidate-id> --json
```

`collect` is an alias for `audit`. Repeat `--operator-key` to review multiple files and `--trust` to select multiple candidate IDs from the review result. Apply requires `--apply --yes` and the exact current `--fingerprint`; `--expected-revision` can also require a specific draft revision. The selected IDs replace the saved trust set, with the exact tested base key retained implicitly. A dry run performs a fresh review and reports no writes or ownership claim; it never applies the selected policy. Review and dry run do not open the game database or modify the supplied key files.

## Example and native evidence

[arma3_key_trust_instance.json](../examples/json/server_backend/arma3_key_trust_instance.json) shows a saved policy for a disposable test key. On the accepted 2.22.154089 server, Windows and Linux runs confirmed that an explicitly trusted key is copied into the runtime's private key folder, required content loads through `-mod`, server-only content through `-serverMod`, optional content is not loaded, removing trust leaves only the base key, and rolling back restores the earlier key set. A Windows run with a regular Steam client confirmed that a client carrying an addon signed by the trusted key joins, while a server without that key refuses it with the engine's "not signed by a key accepted by this server" message.
