# Managed Arma server configuration

For existing vendor files, explicit operator ownership and portable instance packs, see [configuration import/export](Server-Configuration-Import.md).

The desktop server draft editor and `server instance replace --file <instance.json> --expected-revision <revision>` edit the same typed configuration. Start from `server instance show <id> --json`, edit its `data.instance` object, save that object as the input file and replace the reviewed revision. [Configuration snapshots](Server-Admin-Guide.md#configuration-previews-snapshots-and-rollback) describe review, commit and rollback.

The complete [managed instance example](../examples/json/server_backend/arma3_managed_configuration.json) covers every managed policy group. Its rendered counterparts are [dedicated.cfg](../examples/arma3/dedicated.cfg), [network.cfg](../examples/arma3/network.cfg) and [Managed.Arma3Profile](../examples/arma3/Managed.Arma3Profile). They illustrate syntax, not recommended production policy or a runnable mission package.

## Optional overrides

Every managed setting is optional. An unset setting is omitted from the generated file, so the engine keeps its own default; in the desktop editor that is an unchecked setting or a yes/no setting left at **Engine default**. An enabled empty list stays explicit: an empty file-extension list denies all extensions, while omission leaves engine policy intact. Welcome-message lists keep empty entries, which delay the next message.

Decimal fields accept a decimal point or a single decimal comma when typed, and generated files always use a point without exponent notation. Bandwidth is an unsigned whole number of bits per second and accepts values above 32 bits. Foxy refuses non-finite numbers, values outside each field's range, invalid tuple shapes, unsafe filenames and unknown fields, and the editor keeps **Save draft** disabled while any typed value is invalid.

| Settings section | Managed content |
| --- | --- |
| Top-level fields | Hostname, game port, maximum players, persistence/auto-init, BattlEye, signature verification, ordered missions |
| `server` | Credential references and admin IDs, welcome messages, file and HTML access, headless/local IPv4 addresses, voting, connection limits and kick delays, lobby/role/briefing timeouts, gameplay, voice and channels, logging/statistics, idle FPS, mission rotation and downloads, script hooks, `AntiFlood` and `AdvancedOptions` classes |
| `basic` | Minimum/maximum bandwidth, messages per cycle, guaranteed/nonguaranteed payload sizes, update error thresholds, custom-file size, `sockets` packet ceiling |
| `difficulty` | Custom difficulty options, AI level preset and custom AI skill/precision |
| `configuration` | Optional server and basic `.cfg` filenames and the server profile name |
| `mission_cycle[].params` | Named numeric mission parameter overrides; names must be config identifiers |

Array entries follow the engine formats:

| Setting | Entry |
| --- | --- |
| `kick_clients_on_slow_network` | Four flags in the order ping, packet loss, desync, disconnect timeout; `true` kicks, `false` only logs |
| `kick_timeout` | `[cause, delay]`: cause 0 manual kick, 1 connectivity kick, 2 BattlEye kick, 3 harmless kick; delay in seconds, `-1` until mission end, `-2` until server restart |
| `disable_channels` | `[channel, disable text, disable voice]`: channel 0 Global, 1 Side, 2 Command, 3 Group, 4 Vehicle, 5 Direct, 6 System |
| `allowed_vote_cmds` | `[command, before mission start, after mission start]` with an optional vote threshold and side threshold from 0 to 1 |
| `allowed_voted_admin_cmds` | `[command, before mission start, after mission start]`; an empty command means every command |
| `role_timeout` and the other timeout pairs | `[ready seconds, not ready seconds]` |

Packet payloads cannot exceed an explicit socket ceiling, minimum bandwidth cannot exceed maximum, the flood cycle limit cannot exceed its hard limit, and restart and shutdown mission counters cannot both be active. IPv4 lists do not resolve host names. Script hooks are single-line text that Foxy stores and escapes but never evaluates; multi-line or imported hook files are not managed yet. `onDifferentData` is not managed because the accepted 2.22 server binary does not contain it. Quotation marks inside text are doubled and backslashes are kept literally.

## Difficulty and file selection

Foxy passes the committed files through `-config` and `-cfg`, the instance's own profile directory through `-profiles`, and the profile name through `-name` (default `FoxyServer`); port and auto-init remain launch arguments. Profile names use 1-24 ASCII letters, digits, underscores or hyphens. Reserved device names, paths and a server/basic filename collision are refused.

Custom difficulty applies to missions, or a forced difficulty, set to Custom; Recruit, Regular and Veteran use the engine presets, and mission difficulty overrides `forced_difficulty`. The preview warns when missions use Custom without a managed profile (the engine then uses whatever its existing server profile contains) and when a managed profile is unused.

Indicator domains differ: group/friendly/enemy/mine indicators use 0 never, 1 limited distance, 2 always. Commands, waypoints, weapon info and stance use 0 never, 1 fade out, 2 always. Tactical ping is 0 disabled, 1 3D, 2 map, 3 3D and map. Third person view is 0 disabled, 1 enabled, 2 vehicles only. Other options are on/off. AI presets are 0 Low, 1 Normal, 2 High, 3 Custom; only Custom accepts skill and precision values from 0 to 1.

Every managed key is present in the accepted 2.22.154089 server binary, and native activation only accepts that build. The editor shows a minimum version only where Bohemia documents one: file patching 1.50, disconnect timeout 1.56, custom difficulty 1.58, voice codec 1.58, codec quality 1.64, split map content 1.68, mission HTTP downloads 2.20, console timestamps and idle FPS 2.22.

## Secrets and activation

Join, admin and server-command passwords use opaque `secret_<32 lowercase hex digits>` references, never passwords. Previews redact them and native activation refuses any reference until protected credential binding exists. Raw vendor-file import is separate work.

Saving a draft does not change a running or committed runtime: Start keeps using the selected committed generation. Commit the configuration, prepare its deployment and activate it while every consumer of the physical installation is stopped. Immediately before an admitted Start, Foxy replaces only `<profile>.Arma3Profile` inside that instance's own profile directory (`Users\<profile>` on Windows, `home/<profile>` on Linux) with the reviewed profile. The engine's `.vars` namespace files, other profiles, logs, other instances, the adopted installation and older generations are left untouched. Configurations without the new sections keep their earlier files and fingerprints.

Native semantics follow Bohemia's [server configuration](https://community.bohemia.net/wiki/Arma_3%3A_Server_Config_File), [basic configuration](https://community.bohemia.net/wiki/Arma_3%3A_Basic_Server_Config_File), [server profile](https://community.bohemia.net/wiki/Arma_3%3A_Server_Profile) and [difficulty settings](https://community.bohemia.net/wiki/Arma_3%3A_Difficulty_Settings). Mission-level settings, such as a mission's own `disableChannels`, can override server policy.
