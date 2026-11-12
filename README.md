# Surfd Rust

An independent Bevy 0.20 desktop surf game with Rust room and records servers. The 71-map catalog uses Surfd as a reference for controls, movement and timing. Downline and Surf School are built in for offline practice and local server testing. Catalog assets download at runtime.

## Windows download

Download `Surfd-Rust-Windows-x64.exe` from [Releases](https://github.com/davad00/surfd-rust/releases) and open it on a Windows x64 PC. The default renderer is DirectX 12. Set `WGPU_BACKEND=vulkan` to use Vulkan. Downline and Surf School are available offline. Catalog maps download on first use. The game stores settings, local progress and its cache in `%LOCALAPPDATA%\Surfd Rust\maps`.

## Build the client from source

```powershell
cargo run --release --locked
```

**Play now** checks the public room directory and tries up to four rooms with space, starting with the lowest latency. If those rooms fill or become unreachable, it opens solo play. **Servers** refreshes while open and keeps configured rooms visible during a restart. **Maps** (F2) searches the catalog and local BSPs. Catalog maps download collision data and original visual packs on first play and cache them for offline use. **Practice** opens available authored starts, timed bonus routes, and position saves. **Records** shows local results and verified boards when the records service is running.

Open a player profile and select **Runs** to browse their verified personal bests by place or date. **Watch** and **Race** load a compatible replay and its map.

Open **Store** from the main menu or choose **Customize skins and knife** in your profile. Equip Classic, Glacier, or Ember for your avatar and knife. The knife appears in first person. Press **V** with the default bindings to see your avatar and knife in third person. Every choice is free. Signed-in players share their equipped cosmetics with other players in managed rooms and can use the same loadout on another PC. Guest choices are saved on this device.

Windows remembers your sign-in in Credential Manager for the life of the records session, which expires after seven days. Sign out to remove the saved session on this PC.

Press **Esc** while playing to see the current map or bonus leaderboard on the right. Use the play icon to watch a run, the ghost icon to race a main-course run, or **Full leaderboard** to see the rest of that board.

Use WASD and the mouse. Press **V** to show the standing or crouching player model in third person. The camera key and movement bindings can be changed under **Settings**. Bind **Restart current stage** to retry the active stage on an authored staged map. The HUD shows `Stage X/Y` below the timer on staged routes, along with speed, checkpoint progress, strafe sync, splits, and active inputs. Unrestricted starts, manual jump rules, paused runs, and restored positions are excluded from ranked submission. Completed eligible runs can be exported from the finish screen and reopened with `--replay FILE.json`. Verified full runs and individual stage records can be watched from **Records**.

Multiplayer avatars and spectator cameras buffer incoming room poses so motion and crouch transitions play smoothly. Teleports, resets and map changes snap to the new pose. Updated clients and servers compress roster snapshots when both support it.

Open **Surf School** (F7) for seven guided courses covering movement, catching a ramp, air-strafing, transfers, climbing, linking a line, and chaining bunnyhops. **Watch demo** on an unlocked lesson shows a recorded clear with its inputs and section cues. You can pause or step the demo and return to the same attempt. Finish a lesson to earn Bronze and unlock the next one. Replay it to beat your time and earn Silver or Gold. The live coach shows the current section's technique cue and uses your previous clear for target times and checkpoint comparisons. The finish card compares section times and checkpoint speeds with your previous clear and shows a technique cue for the section where you lost the most time. Medal upgrades award local XP once, alongside first-time catalog map and bonus clears. Prompts follow your configured controls, and falls return you to the start. Progress is saved in the client data directory.

Pause during a lesson to practice its current section, or choose any section on the results screen. Each drill starts at the demo's position, speed, and view for that section. *Catch a ramp* and *Switch faces* give steering prompts based on your movement. *Chain bunnyhops* cues the next jump as you approach each pad and accounts for scroll-wheel bindings. Missed bunnyhop and air-strafe drills can report your pad contact time or how much of your turn you paired with the strafe key. Retry the section as often as needed and return to your original attempt. Full lesson clears count toward medals and XP.

After graduation, choose **Start guided Kitsune** to run the original catalog course with opening-ramp feedback, stage progress, and retry cues. Finishes count toward the regular Kitsune personal best and progression. Hide the guide from Pause when you are ready to run on your own. The course path continues through Tendies and Rookie. Surf School, Maps, and the finish cards show your next course based on saved main-course clears.

The main menu downloads Surfd's flythrough clips and rotating posters. FFmpeg on your PATH enables video playback. Turn menu motion off in Settings to use posters. Environmental wind uses the reference's noise, filters, and map presets. **Record Ghost** follows the active run timer and returns to the start when you reset. On catalog maps with a published reference replay, the optional replay tools can convert it once and cache the trajectory under `reference-ghosts` in the client data directory.

The desktop UI embeds the full Fira Mono face so HUD punctuation and menu symbols render offline. Its bundled license is in `assets/fonts/OFL.txt`.

The player model uses textures from Wobble Blocks’ CC0 character pack. [Character attribution](assets/agent/README.md)

The fixed-blade knife, glove, and three finish patterns are original project artwork. The Store renders its previews from the same knife mesh used in the game.

Original Surfd command recordings can be converted with the optional replay tools. Install Bun and set `SURF_REFERENCE_MIRROR` to a separately obtained reference directory containing `config.json` and the original `assets/ghost.worker-*.js`. This enables **Watch original record** in Practice and `--replay FILE.json` for original command recordings. To convert a recording separately, run `bun tools/import-reference-replay.ts FILE.json GEOMETRY.json.gz TRACK.json` and add `--bonus-pack PACK.json.gz` for a bonus. The Rust client opens the resulting `TRACK.json` directly on the matching catalog revision. During playback, Space pauses, Right Arrow steps one command, R restarts, and Backspace exits. `--inspect-replay FILE.json` prints the result without opening a window.

## Run a room and records service locally

Start a room in one terminal.

```powershell
cargo run --release --locked --no-default-features --bin surf-server -- --map downline --rotation downline,surf_boreas
```

Start the accounts and records service in another terminal.

```powershell
cargo run --release --locked --no-default-features --features records-service --bin surf-records
```

For a local client, set `SURF_RECORDS_URL` to `http://127.0.0.1:27166` before launch. Refresh **Find a server** or connect to `127.0.0.1:27165`. Open **Profile & loadout** to create an account on your chosen records service. Guests can surf without signing in. An eligible guest finish can be saved from **Records** after sign-in. The service replays input commands against a trusted map revision before it stores a time. Accounts, sessions, boards, and social data persist in the service's SQLite database.

Press a displayed number on the keyboard or numpad to vote while playing. The panel shows the countdown, map tiers, live totals, and your selection. Use `/nominate`, `/rtv`, `/extend`, and `/vote` in room chat. A managed map ballot with a tied lead opens one final ballot among the tied options.

Use `SURF_SERVERS` for a semicolon-separated list of room addresses. Both the client and the records service read it, and clients can discover the service's list. Set `SURF_RECORDS_URL` to use another records service, or set `SURF_CONNECT` to a room address to join when the client starts. Multiple room processes can listen on distinct ports. The room console accepts `help` for map changes, player listing, mutes, kicks, bans, and shutdown. Rooms relay client movement. Ranked verification is a separate service. Local LAN rooms can use plain TCP. Remote records clients require HTTPS in front of the loopback records service.

Rooms default to Surf. Start a Bhop room with `--mode bhop --map bhop_easy`. Every map in its rotation must match its mode, and managed rooms use maps in the ranked catalog. Install the verified BSP and timer sidecar on both the client and the records service for ranked Source maps. The room roster shows XP and rank for that room's mode.

For a managed room, set `SURF_ROOM_ID` and `SURF_ROOM_TICKET_KEY` on the room process. Give the records process the matching `SURF_ROOM_TICKETS` entry in `room_id|address|key|mode` format. The three-field `room_id|address|key` format defaults to Surf. Use a different random 32-byte key encoded as 64 hex characters for each room. Separate multiple entries with semicolons. The address must match the address the client joins. The records service issues short-lived, single-use tickets and the client obtains one automatically when joining a managed room. Set `SURF_ROOM_MODERATOR_IDS` on the records process to a comma-separated list of native account IDs. Managed guests can play and read chat. Signed-in players get the room permissions associated with their account and verified XP for the room's mode.

For a public managed room, configure a PEM certificate chain and private key with `--tls-cert` and `--tls-key`, or use `SURF_ROOM_TLS_CERT` and `SURF_ROOM_TLS_KEY`. Advertise `tls://hostname:port` in the room directory and ticket configuration. The desktop client verifies the certificate and hostname. Local test deployments can set `SURF_ROOM_CA_CERT` to a trusted development CA. Set `--moderation-file` or `SURF_ROOM_MODERATION_FILE` to choose where a room persists bans, mutes, and reports. Reports retain short excerpts from delivered room chat for up to seven days and are visible to signed moderators.

Rooms reclaim a slot after 20 seconds without a receipt from a client that supports heartbeat receipts. This also catches a client that keeps sending movement after it stops receiving room traffic. Rooms close connections with pending writes after ten seconds without forward progress. Older clients use plain heartbeats.

The [room server performance report](docs/room-performance.md) covers load-test results, including the six-minute 512-player run and the sustained resource budgets.

See the [Linux hosting guide](deploy/README.md) for systemd services, HTTPS proxy configuration, and a desktop connection check.

Set `SURF_RECORDS_URL` to the records service origin and run `bevy-surf --check-room ROOM_ID` from a player network. The command checks records readiness, looks up the room, briefly joins it as a guest using its configured address and certificate, then waits for the disconnect. It prints the room details as JSON on success and exits with an error if a step fails.

In **Friends**, open a conversation and choose **Report** beside a message you received. Choose a reason to send that message for review. Recent received messages remain available under **Past conversations** when a friendship ends. Accounts listed in `SURF_ROOM_MODERATOR_IDS` can open **Review message reports** in Friends and see reports from the past seven days.

Use `GET /v1/ready` to check whether the records database is ready to serve requests. The records service bounds the ranked replay queue and tells clients when to retry a busy submission. The desktop client retries temporary ranked ticket failures for the same run and account, even after an eligible finish. Signed players in managed rooms also retry temporary lease-renewal failures during a records-service outage. For a public HTTPS reverse proxy, buffer request bodies and cap them at 16 MiB. Set `SURF_RECORDS_TRUST_REAL_IP=1` only when the proxy connects from loopback and replaces the incoming `X-Real-IP` header with the client address.

When a ranked save loses its response, the client checks for the existing result before re-uploading. The service keeps the original result for 24 hours, tied to the account and replay. Once the service reserves the original attempt, a pending save can resume after a desktop restart when you sign in to the same account and records service. The local queue keeps the replay and account reference for up to 24 hours and contains no login or attempt tokens.

Run `surf-records --data DIRECTORY --backup FILE` to make a checked snapshot while the records service is running. Use a new filename on a filesystem that supports hard links. To verify a recovery copy, put the snapshot in a separate data directory as `records.sqlite3` and start the records service with that directory and a separate loopback port. [SQLite snapshot behavior](https://sqlite.org/lang_vacuum.html)

Google and X sign-in are available when configured on the records service. Set `SURF_OAUTH_PUBLIC_URL` to its public HTTPS origin. Set `SURF_GOOGLE_CLIENT_ID` and `SURF_GOOGLE_CLIENT_SECRET`, or `SURF_X_CLIENT_ID` and `SURF_X_CLIENT_SECRET`, on that service. Register the callback at `/v1/oauth/google/callback` or `/v1/oauth/x/callback` with the corresponding provider. The desktop client opens the provider's sign-in page and receives the native session after authorization.

In the desktop **Profile & loadout**, sign in and open **Surfd.net history**. On Surfd.net, use **Profile & loadout → Profile → Copy profile link**, then choose **Paste link → Load history** in the desktop. This imports the public username, points, rank, map scores and run history. **Map scores** shows the original completion and performance awards, and **Splits** reads the selected run's checkpoints. Loading history again refreshes the snapshot. Exact usernames in Surfd's public top 100 also work.

The snapshot is saved on this device for the current desktop account and records service. After desktop sign-in, reopening it reads the local cache, and **Forget history** removes it. Successful desktop account deletion also removes that account's local snapshot. The imported totals retain their Surfd.net attribution. Full account linking, private profile fields and shared ranked progression require an authorized integration with Surfd's account service. Its published progression uses points and rank. Replay retention depends on the original service. [Surfd account and replay storage](https://surfd.net/privacy.html)

## Map data

Catalog maps use Surfd's converted collision, base textures, baked lighting, sky depth, and authored prop, area and proximity fades where supplied. In-game map loading uses original GPU-compressed material textures when the adapter supports them and can fall back to decoded packs. The project bundles catalog metadata and fetches map assets into a local cache. Map and asset credits remain with their authors. [Surfd credits](https://surfd.net/credits.html)

Catalog bonus routes apply their own collision changes, teleport rules, boosts, and trigger outputs. Imported bonuses use a revised leaderboard ruleset, and previous desktop bonus recordings can still play with their recorded rules.

Put local maps in the client data directory, `maps` beside the executable, or a directory listed in `SURF_MAPS`. Use **Rescan local maps** after adding files. The desktop loader detects Source 1, GoldSrc, Quake and QuakeWorld BSPs, and Quake III BSPs or PK3s. Use `--map FILE` to open a map directly for testing. Imported maps use the desktop movement rules.

Point `SURF_SOURCE_ASSETS` to a Source installation or `_dir.vpk` to resolve stock textures. Point `SURF_GOLDSRC_ASSETS` to GoldSrc WAD files or directories. Quake BSPs need a palette for original colors and look for a matching KTX `.route` file for authored timer zones. You can set the palette file with `SURF_QUAKE_PALETTE`. Quake III imports support DeFRaG timer targets, embedded textures and lightmaps, static brush collision, and curved patch rendering.

Use `--inspect FILE.bsp` for import counts, `--catalog boreas` to open a catalog map, or `--inspect-catalog boreas` to cache and inspect it. Use `--surfd GEOMETRY [VISUAL-MANIFEST]` for local converted files with the visual packs alongside the manifest. Set `SURF_DATA` to change the client map, settings, records, and cache directory. The records service accepts `--data` for its database and `--maps` for its trusted geometry cache.

Open **Practice** (F4) → **Local maps** to browse verified imports. A matching `<map>.surf.json` supplies authored Source starts, finishes, checkpoints, and bonuses. The game checks the BSP checksum before using those zones. Runs opened from **Local maps** use local times.

The map selector has separate Surf and Bhop tabs with published tiers. Local imports need a positive published tier in their metadata, an authored start and finish for every route, and any checkpoints or stage boundaries the author provided. Compressed `<map>.bsp.gz` files use the same sidecar names as uncompressed maps. The pinned `bhop_easy` revision can be used for ranked play when both the client and records service have its verified BSP and timer file.

## Verification

Movement was compared against original recordings for all 71 catalog main routes and 160 bonuses, using both recorded and current rules. The audit matched 1,119,932 position and HUD speed samples, 640 displayed checkpoint speeds, and all finish frames. The largest raw velocity difference was about 0.000122 units/s. Finish and split time differences were below 0.00000000000003 seconds.

```powershell
cargo test --locked --no-default-features --features records-service
```

The opt-in `surfd_parity` test compares native movement with a recorded Surfd run using its original command format. Set `SURFD_PARITY_GEOMETRY` to its collision pack, `SURFD_PARITY_REPLAY` to the command replay, and `SURFD_PARITY_GHOST` to its converted first-person track. Set `SURFD_PARITY_BONUS_PACK` for a bonus route. Run `cargo test --locked --no-default-features --test surfd_parity -- --ignored --nocapture` to see frame, split, and finish differences. Set `SURFD_PARITY_LIVE_RULES=1` to test the current desktop rules with the same commands.

Set `SURFD_VISUAL_MANIFESTS` to a directory of original manifests named `<map-slug>.gz` and run `cargo test --locked --no-default-features --test visual_catalog -- --ignored --nocapture` to check the full catalog's native manifest and visual-pack selection.

Use `--bench-replay TRACK.json` to render a desktop replay to a 1280×800 GPU image and print application frame intervals, map load time, and texture payload as JSON. The benchmark disables environmental audio, warms up for three seconds, samples for twelve seconds, and exits automatically. `max_frame_ms` covers both warmup and sampling. `sample_max_frame_ms` covers the sampling interval. Set `SURF_BENCH_DECODED=1` to compare decoded textures against the adapter's normal selection. For a screenshot at a fixed replay frame, set `SURF_BENCH_START_TICK`, `SURF_BENCH_HOLD_TICK=1`, and `SURF_BENCH_SCREENSHOT` to a `.png` path.

Set `SURF_BENCH_AVATAR_COUNT` from 1 to 63 to place that many players in front of the initial replay camera. Hold the replay frame to compare the same crowd view between runs. Set `SURF_BENCH_SERIAL_PACKS=1` to compare serial map pack inflation with the bounded worker path.

Set `SURF_BENCH_ALL_LODS=1` to compare Boreas with off-screen model detail updates enabled.

Set `SURF_BENCH_WIDE_VERTICES=1` to compare the previous 56-byte map vertex layout with the compact 48-byte layout.

Set `SURF_BENCH_UNGROUPED_PVS=1` to compare map visibility updates with one check per draw. Replay reports include visibility transitions, checks, and CPU time for maps with a visibility table.

Set `SURF_BENCH_SHARED_MATERIALS=1` to compare the experimental shared opaque shader, or `SURF_BENCH_SPECIALIZED_MATERIALS=1` to compare a separate pipeline for eligible opaque materials. `SURF_BENCH_RENDER_DIAGNOSTICS=1` adds opaque-pass GPU timing and fragment counts when supported by the adapter. Measure application frame rate in a separate run with diagnostics off.

Set `SURF_BENCH_RENDER_TRACE=1` and `RUST_LOG=bevy_surf::bench_render_trace=info` to log slow frame preparation, pipeline queues, and render submission during a replay benchmark.

Set `SURF_BENCH_COPY_TEXTURE_UPLOADS=1` to compare cloning map texture payloads with transferring them into Bevy's image assets. Add `RUST_LOG=warn,bevy_surf::world=debug` to include upload preparation time and copied bytes.

Set `SURF_BENCH_GENERIC_MATERIALS=1` to compare the general map shader with the dedicated opaque RGBE shader.

On Windows, set `WGPU_BACKEND=vulkan` or `WGPU_BACKEND=dx12` to select a renderer explicitly. Windows x64 MSVC builds link DXC into the executable. The first source build needs network access, `curl`, and `tar` to fetch and verify the compiler archive. Legacy FXC serializes pipeline compilation and can pause as new shaders load. [DXC license notices](licenses/dxc)

To build the single-file Windows release with the C and C++ runtimes linked in, run the following from a Visual Studio build environment with `lld-link` available.

```powershell
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static -C linker=lld-link.exe'
cargo rustc --locked --release --target x86_64-pc-windows-msvc --bin bevy-surf -- -C link-arg=/SUBSYSTEM:WINDOWS -C link-arg=/ENTRY:mainCRTStartup
```

The project targets Bevy desktop clients and dedicated servers. The records service verifies native ranked runs using the same Rust simulation and Source-precision command yaw as live play. Original replay imports use the reference simulator for playback. Native movement checks cover one original main-course recording per catalog map. Full-catalog pixel comparisons remain open. The project runs its own accounts and room directory.

Native ranked runs use replay version 2. Catalog boards use the updated Surfd collision, landing, and trigger rules. Archived desktop runs still play with their board's physics profile. Account progression keeps the higher score earned on each map across the current and preceding board for the same geometry.
